//! Ports of ElectroDB's expression builders (operations.js, update.js,
//! where.js): placeholder naming, update expressions and condition/filter
//! expressions. Placeholders are generated exactly as ElectroDB generates
//! them, so requests can be compared with recorded ones.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use regex::Regex;

use crate::model::Attribute;
use crate::value::{Attrs, Value, is_nan};

static NON_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^0-9A-Za-z_]").unwrap());
static NEWLINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n|\r").unwrap());
static EMPTY_CHARS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n|\r|[0-9A-Za-z_]").unwrap());

/// Mirrors `formatExpressionName` in operations.js.
pub(crate) fn format_expression_name(name: &str, seen: &mut HashMap<String, String>) -> String {
    let name_was_not_a_number = is_nan(name);
    let mut formatted = NON_WORD.replace_all(name, "").into_owned();
    if formatted.is_empty() {
        formatted = "p".into();
    } else if name_was_not_a_number != is_nan(&formatted) {
        formatted = format!("p{formatted}");
    }
    let original = formatted.clone();
    let mut suffix = 1;
    loop {
        match seen.get(&formatted) {
            Some(prev) if prev != name => {
                suffix += 1;
                formatted = format!("{original}_{suffix}");
            }
            _ => break,
        }
    }
    seen.insert(formatted.clone(), name.to_string());
    formatted
}

/// A name registered for an attribute path: its JSON path and its
/// expression form.
#[derive(Clone, Debug, Default)]
pub(crate) struct PathNames {
    pub json: String,
    pub expression: String,
    pub prop: String,
}

/// A value placeholder recorded for an attribute path.
#[derive(Clone, Debug)]
pub(crate) struct PathValue {
    pub value: Value,
    pub name: String,
}

/// Mirrors ExpressionState.
#[derive(Clone, Default)]
pub(crate) struct ExprState {
    pub prefix: String,
    pub names: IndexMap<String, String>,
    pub values: IndexMap<String, Value>,
    pub paths: IndexMap<String, PathValue>,
    pub counts: HashMap<String, usize>,
    pub impacted: BTreeMap<String, String>,
    pub refs: BTreeMap<String, Arc<Attribute>>,
    pub seen_names: HashMap<String, String>,
}

impl ExprState {
    pub fn new(prefix: &str) -> Self {
        ExprState {
            prefix: prefix.into(),
            ..Default::default()
        }
    }

    pub fn set_name(&mut self, parent: Option<&PathNames>, name: &str, value: &str) -> PathNames {
        let name = format_expression_name(name, &mut self.seen_names);
        let prop = format!("#{name}");
        match parent {
            None => {
                self.add_name(&prop, value);
                PathNames {
                    json: name.clone(),
                    expression: prop.clone(),
                    prop,
                }
            }
            Some(parent) if is_nan(&name) => {
                self.add_name(&prop, value);
                PathNames {
                    json: format!("{}.{name}", parent.json),
                    expression: format!("{}.{prop}", parent.expression),
                    prop,
                }
            }
            Some(parent) => PathNames {
                json: format!("{}[{name}]", parent.json),
                expression: format!("{}[{name}]", parent.expression),
                prop,
            },
        }
    }

    pub fn add_name(&mut self, prop: &str, value: &str) {
        self.names.insert(prop.to_string(), value.to_string());
    }

    pub fn set_value(&mut self, name: &str, value: Value) -> String {
        let formatted = format_expression_name(name, &mut self.seen_names);
        let n = self.counts.get(&formatted).copied().unwrap_or(0);
        self.counts.insert(formatted.clone(), n + 1);
        let expr = format!(":{formatted}{}{n}", self.prefix);
        self.put_value(&expr, value);
        expr
    }

    pub fn put_value(&mut self, expr: &str, value: Value) {
        self.values.insert(expr.to_string(), value);
    }

    pub fn set_path(&mut self, path: &str, v: PathValue) {
        self.paths.insert(path.to_string(), v);
    }

    pub fn values_attrs(&self) -> Attrs {
        self.values
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[derive(Clone)]
struct SeenSet {
    name: PathNames,
    value: String,
    expression: String,
}

const UPDATE_OP_ORDER: [&str; 5] = ["set", "remove", "add", "subtract", "delete"];

/// Mirrors UpdateExpression.
#[derive(Clone)]
pub(crate) struct UpdateExpr {
    pub state: ExprState,
    ops: HashMap<String, Vec<String>>,
    pub composites: Attrs,
    pub comp_order: Vec<String>,
    seen: HashMap<String, SeenSet>,
}

impl UpdateExpr {
    pub fn new() -> Self {
        UpdateExpr {
            state: ExprState::new("_u"),
            ops: HashMap::new(),
            composites: Attrs::new(),
            comp_order: Vec::new(),
            seen: HashMap::new(),
        }
    }

    pub fn add(&mut self, op: &str, expression: &str) {
        let list = self.ops.entry(op.to_string()).or_default();
        if !list.iter().any(|e| e == expression) {
            list.push(expression.to_string());
        }
    }

    fn unadd(&mut self, op: &str, expression: &str) {
        if let Some(list) = self.ops.get_mut(op)
            && let Some(i) = list.iter().position(|e| e == expression)
        {
            list.remove(i);
        }
    }

    pub fn add_composite(&mut self, name: &str, value: &Value) -> bool {
        if value.is_undefined() {
            return false;
        }
        match self.composites.get(name) {
            None => {
                self.composites.insert(name.to_string(), value.clone());
                self.comp_order.push(name.to_string());
                true
            }
            Some(prev) => prev == value,
        }
    }

    /// Mirrors UpdateExpression.set: a SET on a top-level field, replacing an
    /// earlier set of the same field.
    pub fn set(&mut self, name: &str, value: Value, if_not_exists: bool) {
        let (n, v) = if let Some(s) = self.seen.get(name).cloned() {
            self.unadd("set", &s.expression);
            (s.name, s.value)
        } else {
            let n = self.state.set_name(None, name, name);
            let v = self.state.set_value(name, value);
            (n, v)
        };
        let expression = if if_not_exists {
            format!("{} = if_not_exists({}, {v})", n.prop, n.prop)
        } else {
            format!("{} = {v}", n.prop)
        };
        self.seen.insert(
            name.to_string(),
            SeenSet {
                name: n,
                value: v,
                expression: expression.clone(),
            },
        );
        self.add("set", &expression);
    }

    pub fn remove(&mut self, name: &str) {
        let n = self.state.set_name(None, name, name);
        self.add("remove", &n.prop);
    }

    pub fn build(&self) -> String {
        let mut parts = Vec::new();
        for op in UPDATE_OP_ORDER {
            if let Some(list) = self.ops.get(op)
                && !list.is_empty()
            {
                parts.push(format!("{} {}", op.to_uppercase(), list.join(", ")));
            }
        }
        parts.join(" ")
    }
}

/// Mirrors FilterExpression (where.js).
#[derive(Clone, Default)]
pub(crate) struct FilterExpr {
    pub state: ExprState,
    pub expression: String,
}

impl FilterExpr {
    pub fn new() -> Self {
        FilterExpr {
            state: ExprState::new(""),
            expression: String::new(),
        }
    }

    pub fn add_expr(&mut self, expression: &str, as_prefix: bool) {
        let mut existing = self.expression.clone();
        if existing.is_empty() {
            self.expression = NEWLINES.replace_all(expression, "").trim().to_string();
            return;
        }
        let expression = NEWLINES.replace_all(expression, "").trim().to_string();
        if EMPTY_CHARS.replace_all(&expression, "").trim().is_empty() {
            return;
        }
        if !as_prefix && !existing.starts_with('(') && !existing.ends_with(')') {
            existing = format!("({existing})");
        }
        self.expression = if as_prefix {
            format!("({expression}) AND {existing}")
        } else {
            format!("{existing} AND {expression}")
        };
    }

    /// Mirrors FilterExpression.unsafeSet: a condition on a top-level field,
    /// without attribute checks.
    pub fn unsafe_set(&mut self, as_prefix: bool, op: &str, name: &str, values: &[Value]) {
        let n = self.state.set_name(None, name, name);
        let refs: Vec<String> = values
            .iter()
            .map(|v| self.state.set_value(name, v.clone()))
            .collect();
        let expr = filter_template(op, &n.prop, &refs);
        self.add_expr(&expr, as_prefix);
    }
}

/// Mirrors the templates in filterOperations.js. Unknown operations render
/// as an empty expression.
pub(crate) fn filter_template(op: &str, name: &str, values: &[String]) -> String {
    let v = |i: usize| values.get(i).map(String::as_str).unwrap_or("undefined");
    match op {
        "size" => format!("size({name})"),
        "type" => format!("attribute_type({name}, {})", v(0)),
        "ne" => format!("{name} <> {}", v(0)),
        "eq" => format!("{name} = {}", v(0)),
        "gt" => format!("{name} > {}", v(0)),
        "lt" => format!("{name} < {}", v(0)),
        "gte" => format!("{name} >= {}", v(0)),
        "lte" => format!("{name} <= {}", v(0)),
        "between" => format!("({name} between {} and {})", v(0), v(1)),
        "begins" => format!("begins_with({name}, {})", v(0)),
        "exists" => format!("attribute_exists({name})"),
        "notExists" => format!("attribute_not_exists({name})"),
        "contains" => format!("contains({name}, {})", v(0)),
        "notContains" => format!("not contains({name}, {})", v(0)),
        "eqOrNotExists" => format!("({name} = {} OR attribute_not_exists({name}))", v(0)),
        "value" => v(0).to_string(),
        "name" => name.to_string(),
        _ => String::new(),
    }
}

/// Reports whether `op` is a filter operation.
pub(crate) fn is_filter_op(op: &str) -> bool {
    matches!(
        op,
        "size"
            | "type"
            | "ne"
            | "eq"
            | "gt"
            | "lt"
            | "gte"
            | "lte"
            | "between"
            | "begins"
            | "exists"
            | "notExists"
            | "contains"
            | "notContains"
            | "eqOrNotExists"
            | "value"
            | "name"
    )
}
