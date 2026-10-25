//! Attribute paths and the operations of ElectroDB's attribute proxies.

use std::fmt;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::expr::{ExprState, PathNames, PathValue, UpdateExpr, filter_template, is_filter_op};
use crate::model::{AttrType, Attribute, Model, format_casing};
use crate::value::{Number, Value, is_nan, js_string};

/// Addresses an attribute: a root attribute name, then map property names
/// or list indexes (`"0"`), as in ElectroDB's attribute proxies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Path(pub Vec<String>);

impl Path {
    /// Mirrors ElectroDB's `parseJSONPath`: `a.b[0].c` → `[a b 0 c]`.
    pub fn parse(s: &str) -> Path {
        let s = s.replace('[', ".").replace(']', "");
        Path(
            s.split('.')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect(),
        )
    }
}

impl fmt::Display for Path {
    /// Renders the path the way ElectroDB's JSON paths look.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, part) in self.0.iter().enumerate() {
            if i == 0 {
                f.write_str(part)?;
            } else if !is_nan(part) {
                write!(f, "[{part}]")?;
            } else {
                write!(f, ".{part}")?;
            }
        }
        Ok(())
    }
}

/// A path resolved against the model, mirroring the chain of pathProxy
/// builds.
pub(crate) struct Resolved {
    pub target: Option<Arc<Attribute>>,
    steps: Vec<(String, String)>,
}

/// Walks a path to its attribute. Unknown root attributes return no target
/// (ElectroDB skips them); unknown nested ones are an error.
pub(crate) fn resolve(m: &Model, p: &Path) -> Result<Resolved> {
    let Some(first) = p.0.first() else {
        return Err(Error::invalid("empty attribute path"));
    };
    let Some(root) = m.attribute(first) else {
        return Ok(Resolved {
            target: None,
            steps: vec![],
        });
    };
    let mut target = root.clone();
    let mut steps = vec![(root.name.clone(), root.field.clone())];
    for prop in &p.0[1..] {
        let Some(child) = child_attribute(&target, prop) else {
            return Err(Error::invalid(format!(
                "invalid attribute {prop:?} at path {:?}",
                p.to_string()
            )));
        };
        let mut field = child.field.clone();
        if child.ty == AttrType::Any && child.name != *prop {
            field = prop.clone(); // nested under an "any" attribute
        }
        steps.push((prop.clone(), field));
        target = child;
    }
    Ok(Resolved {
        target: Some(target),
        steps,
    })
}

/// Mirrors `Attribute.getChild`.
fn child_attribute(a: &Arc<Attribute>, prop: &str) -> Option<Arc<Attribute>> {
    if a.ty == AttrType::Any {
        return Some(a.clone());
    }
    if !is_nan(prop) && matches!(a.ty, AttrType::List | AttrType::Set) {
        return a.items.clone();
    }
    a.property(prop).cloned()
}

impl Resolved {
    /// Registers the path's names, as the proxy's `commit()` does.
    fn commit(&self, s: &mut ExprState) -> PathNames {
        let mut cur: Option<PathNames> = None;
        for (prop, field) in &self.steps {
            cur = Some(s.set_name(cur.as_ref(), prop, field));
        }
        cur.unwrap_or_default()
    }
}

/// Mirrors `Attribute.acceptable` (and SetAttribute's).
fn acceptable(a: &Attribute, v: &Value) -> bool {
    if a.ty == AttrType::Set {
        return match v {
            Value::List(l) | Value::Set(l) => !l.is_empty(),
            v => !v.is_undefined(),
        };
    }
    !v.is_undefined()
}

/// Mirrors `Attribute.applyFixings`: values compared with a key field
/// attribute get the key template's prefix/postfix and casing.
fn apply_fixings(a: &Attribute, v: Value) -> Value {
    if v.is_undefined() {
        return v;
    }
    let mut v = v;
    if (a.ty == AttrType::String || a.ty == AttrType::Enum)
        && (!a.key_prefix.is_empty() || !a.key_postfix.is_empty())
    {
        v = Value::String(format!(
            "{}{}{}",
            a.key_prefix,
            js_string(&v),
            a.key_postfix
        ));
    }
    if let (Value::String(s), Some(c)) = (&v, a.key_casing) {
        return Value::String(format_casing(s, c));
    }
    v
}

/// Mirrors `template.length > 3` for each operation.
fn takes_value(op: &str) -> bool {
    matches!(
        op,
        "ifNotExists"
            | "value"
            | "append"
            | "add"
            | "subtract"
            | "set"
            | "del"
            | "delete"
            | "type"
            | "ne"
            | "eq"
            | "gt"
            | "lt"
            | "gte"
            | "lte"
            | "between"
            | "begins"
            | "contains"
            | "notContains"
            | "eqOrNotExists"
    )
}

/// Where an operation registers its names and values.
pub(crate) enum OpTarget<'a> {
    Filter(&'a mut ExprState),
    Update(&'a mut UpdateExpr),
}

/// Mirrors the operation functions built by AttributeOperationProxy: it
/// registers names and values and returns the rendered expression. Update
/// operations are also added to the update; it returns `""` when ElectroDB
/// would skip the operation (e.g. an empty set).
pub(crate) fn apply_op(
    m: &Model,
    target: OpTarget<'_>,
    op: &str,
    p: &Path,
    values: &[Value],
) -> Result<String> {
    let r = resolve(m, p)?;
    let Some(attr) = r.target.clone() else {
        return Ok(String::new());
    };
    let fixed: Vec<Value> = values
        .iter()
        .map(|v| apply_fixings(&attr, v.clone()))
        .filter(|v| !v.is_undefined())
        .collect();
    let is_filter = matches!(target, OpTarget::Filter(_));
    let accept = fixed.iter().all(|v| acceptable(&attr, v));
    if !is_filter && takes_value(op) && (!accept || fixed.is_empty()) {
        return Ok(String::new());
    }
    if is_filter && !is_filter_op(op) {
        return Err(Error::invalid(format!("unknown filter operation {op:?}")));
    }

    let mut target = target;
    let state: &mut ExprState = match &mut target {
        OpTarget::Filter(s) => s,
        OpTarget::Update(u) => &mut u.state,
    };
    let ps = r.commit(state);
    let mut refs = Vec::with_capacity(fixed.len());
    for v in fixed {
        let name = state.set_value(&attr.name, v.clone());
        state.set_path(
            &ps.json,
            PathValue {
                value: v,
                name: name.clone(),
            },
        );
        refs.push(name);
    }

    match target {
        OpTarget::Filter(state) => {
            state.impacted.insert(ps.json.clone(), op.to_string());
            state.refs.insert(ps.json.clone(), attr.clone());
            Ok(filter_template(op, &ps.expression, &refs))
        }
        OpTarget::Update(upd) => {
            let (kind, expression) =
                update_template(op, &attr, &ps.expression, &refs, &mut |name, v| {
                    upd.state.set_value(&format!("{}_{name}", attr.name), v)
                })?;
            upd.state.impacted.insert(ps.json.clone(), op.to_string());
            upd.state.refs.insert(ps.json.clone(), attr.clone());
            if !kind.is_empty() {
                upd.add(kind, &expression);
            }
            Ok(expression)
        }
    }
}

/// Mirrors updateOperations.js, returning the update section (`"set"`,
/// `"add"`, ...) and the expression.
fn update_template(
    op: &str,
    a: &Attribute,
    path: &str,
    values: &[String],
    create_value: &mut dyn FnMut(&str, Value) -> String,
) -> Result<(&'static str, String)> {
    let v = |i: usize| {
        values
            .get(i)
            .map(String::as_str)
            .unwrap_or("undefined")
            .to_string()
    };
    Ok(match op {
        "ifNotExists" => ("set", format!("{path} = if_not_exists({path}, {})", v(0))),
        "append" => {
            if a.ty != AttrType::Any && a.ty != AttrType::List {
                return Err(Error::invalid(
                    r#"invalid update attribute operation: "APPEND" can only be performed on attributes with type "list" or "any""#,
                ));
            }
            let dv = create_value("default_value", Value::List(vec![]));
            (
                "set",
                format!(
                    "{path} = list_append(if_not_exists({path}, {dv}), {})",
                    v(0)
                ),
            )
        }
        "add" => match a.ty {
            // The value is a placeholder string, so ElectroDB's "any holding a
            // number" check never matches: any always ADDs.
            AttrType::Any | AttrType::Set | AttrType::Number => ("add", format!("{path} {}", v(0))),
            _ => {
                return Err(Error::invalid(
                    r#"invalid update attribute operation: "ADD" can only be performed on attributes with type "number", "set", or "any""#,
                ));
            }
        },
        "subtract" => {
            if a.ty != AttrType::Any && a.ty != AttrType::Number {
                return Err(Error::invalid(
                    r#"invalid update attribute operation: "SUBTRACT" can only be performed on attributes with type "number" or "any""#,
                ));
            }
            let dv = create_value("default_value", Value::Number(Number::new("0")));
            (
                "set",
                format!("{path} = (if_not_exists({path}, {dv}) - {})", v(0)),
            )
        }
        "set" => ("set", format!("{path} = {}", v(0))),
        "remove" => ("remove", path.to_string()),
        "delete" | "del" => {
            if a.ty != AttrType::Any && a.ty != AttrType::Set {
                return Err(Error::invalid(
                    r#"invalid update attribute operation: "DELETE" can only be performed on attributes with type "set" or "any""#,
                ));
            }
            ("delete", format!("{path} {}", v(0)))
        }
        "name" => ("", path.to_string()),
        "value" => ("", v(0)),
        _ => return Err(Error::invalid(format!("unknown update operation {op:?}"))),
    })
}
