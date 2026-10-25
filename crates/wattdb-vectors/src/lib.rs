//! Loads the ElectroDB golden vectors recorded by `tools/recordvectors`
//! (decisions D8, D52–D55) for wattdb's tests, and resolves their named
//! callbacks the way `tools/recordvectors/registry.mjs` does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use regex::RegexBuilder;
use serde_json::Value as Json;
use wattdb_engine::model::{
    AttrType, AttributeDef, ConditionFn, DefaultFn, Definition, IndexDef, IndexType, KeyDef,
    Padding, Projection, TransformFn, ValidateFn,
};
use wattdb_engine::value::{js_length, js_string};
use wattdb_engine::{Attrs, Model, Value, wire};

/// One recorded scenario.
#[derive(Clone, Debug)]
pub struct Vector {
    pub file: PathBuf,
    /// The file name without `.json`.
    pub name: String,
    pub table: String,
    /// The scenario's entity schemas, in order.
    pub entities: Vec<Entity>,
    /// Service names and their entity aliases, in order.
    pub services: Vec<(String, Vec<String>)>,
    pub cases: Vec<Case>,
}

/// A named ElectroDB schema from a scenario.
#[derive(Clone, Debug)]
pub struct Entity {
    pub name: String,
    pub schema: Json,
}

/// One recorded case.
#[derive(Clone, Debug, Default)]
pub struct Case {
    pub name: String,
    pub entity: String,
    pub service: String,
    pub chain: Vec<Step>,
    /// The options the terminals were called with, when given.
    pub params_options: Option<Json>,
    pub go_options: Option<Json>,
    /// The scripted wire responses for `.go()`.
    pub responses: Vec<Json>,
    /// The wire form of `.params()`; `None` when skipped or on error.
    pub params: Option<Vec<Request>>,
    pub params_error: Option<JsError>,
    /// `None` when `.go()` wasn't run.
    pub go: Option<Go>,
    pub value: Option<Json>,
    /// Set when building the chain threw.
    pub error: Option<JsError>,
}

/// One chain call.
#[derive(Clone, Debug)]
pub struct Step {
    pub method: String,
    pub args: Vec<Json>,
}

/// One request body sent to DynamoDB.
#[derive(Clone, Debug)]
pub struct Request {
    pub target: String,
    pub request: Json,
}

/// The recorded outcome of `.go()`.
#[derive(Clone, Debug, Default)]
pub struct Go {
    pub result: Option<Json>,
    pub error: Option<JsError>,
    pub requests: Vec<Request>,
}

/// A recorded JS error.
#[derive(Clone, Debug, Default)]
pub struct JsError {
    pub name: String,
    pub message: String,
}

/// `testdata/vectors` in the repository root.
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/vectors")
}

/// The gitignored `private/vectors` directory in the repository root. It
/// may not exist.
pub fn private_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../private/vectors")
}

/// Loads the committed vectors plus any private ones.
pub fn load_all() -> Vec<Vector> {
    let mut out = load_dir(&dir());
    if private_dir().is_dir() {
        out.extend(load_dir(&private_dir()));
    }
    out
}

/// Loads every `*.json` vector in `dir`, sorted by name.
pub fn load_dir(dir: &Path) -> Vec<Vector> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files.iter().map(|f| load(f)).collect()
}

/// Loads `testdata/vectors/<name>.json`.
pub fn load_named(name: &str) -> Vector {
    load(&dir().join(format!("{name}.json")))
}

/// Reads one vector file.
pub fn load(path: &Path) -> Vector {
    let data = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let raw: Json =
        serde_json::from_slice(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let name = path.file_stem().unwrap().to_string_lossy().into_owned();
    let mut v = Vector {
        file: path.to_path_buf(),
        name,
        table: raw["table"].as_str().unwrap_or("table").to_string(),
        entities: Vec::new(),
        services: Vec::new(),
        cases: Vec::new(),
    };
    if let Some(ents) = raw["entities"].as_object() {
        for (k, s) in ents {
            v.entities.push(Entity {
                name: k.clone(),
                schema: s.clone(),
            });
        }
    }
    if let Some(svcs) = raw["services"].as_object() {
        for (k, s) in svcs {
            let names = s["entities"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            v.services.push((k.clone(), names));
        }
    }
    for c in raw["cases"].as_array().cloned().unwrap_or_default() {
        v.cases.push(parse_case(&c));
    }
    v
}

fn js_error(j: &Json) -> Option<JsError> {
    if !j.is_object() {
        return None;
    }
    Some(JsError {
        name: j["name"].as_str().unwrap_or_default().to_string(),
        message: j["message"].as_str().unwrap_or_default().to_string(),
    })
}

fn requests(j: &Json) -> Vec<Request> {
    j.as_array()
        .map(|a| {
            a.iter()
                .map(|r| Request {
                    target: r["target"].as_str().unwrap_or_default().to_string(),
                    request: r["request"].clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_case(c: &Json) -> Case {
    let s = |k: &str| c[k].as_str().unwrap_or_default().to_string();
    let mut out = Case {
        name: s("name"),
        entity: s("entity"),
        service: s("service"),
        chain: parse_chain(&c["chain"]),
        responses: c["responses"].as_array().cloned().unwrap_or_default(),
        ..Default::default()
    };
    if c["params"].is_object() {
        out.params_options = Some(c["params"].clone());
    }
    if c["go"].is_object() {
        out.go_options = Some(c["go"].clone());
    }
    let rec = &c["recorded"];
    if rec["go"].is_object() {
        let g = &rec["go"];
        out.go = Some(Go {
            result: if g["result"].is_null() {
                None
            } else {
                Some(g["result"].clone())
            },
            error: js_error(&g["error"]),
            requests: requests(&g["requests"]),
        });
    }
    if !rec["value"].is_null() {
        out.value = Some(rec["value"].clone());
    }
    out.error = js_error(&rec["error"]);
    match &rec["params"] {
        Json::Array(_) => out.params = Some(requests(&rec["params"])),
        Json::Object(_) => out.params_error = js_error(&rec["params"]["error"]),
        _ => {}
    }
    out
}

/// Parses chain steps, each `[method, args...]`.
pub fn parse_chain(steps: &Json) -> Vec<Step> {
    steps
        .as_array()
        .map(|a| {
            a.iter()
                .map(|step| {
                    let parts = step.as_array().expect("chain step");
                    Step {
                        method: parts[0].as_str().expect("chain method").to_string(),
                        args: parts[1..].to_vec(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Case {
    /// The case's recorded requests: the params when recorded, otherwise the
    /// requests `.go()` sent.
    pub fn requests(&self) -> &[Request] {
        if let Some(p) = &self.params {
            return p;
        }
        self.go
            .as_ref()
            .map(|g| g.requests.as_slice())
            .unwrap_or(&[])
    }
}

/// Decodes plain JSON into engine values (numbers keep their JSON text).
pub fn value(j: &Json) -> Value {
    wire::from_plain_json(j)
}

/// Decodes a JSON object into engine attribute values.
pub fn attrs(j: &Json) -> Attrs {
    match value(j) {
        Value::Map(m) => m,
        v => panic!("expected an object, got {v:?}"),
    }
}

/// A JSON object's keys in document order.
pub fn keys(j: &Json) -> Vec<String> {
    j.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// Decodes a JSON object of wire attribute values.
pub fn attribute_map(j: &Json) -> Attrs {
    wire::attrs_from_json(j).unwrap_or_else(|e| panic!("{e}: {j}"))
}

/// Decodes one wire attribute value.
pub fn attribute_value(j: &Json) -> Value {
    wire::from_json(j).unwrap_or_else(|e| panic!("{e}: {j}"))
}

impl Vector {
    /// Builds every entity's model, keyed by its name.
    pub fn models(&self) -> Vec<(String, Model)> {
        self.entities
            .iter()
            .map(|e| {
                (
                    e.name.clone(),
                    e.model()
                        .unwrap_or_else(|err| panic!("{}: entity {}: {err}", self.name, e.name)),
                )
            })
            .collect()
    }
}

impl Entity {
    /// Converts the embedded ElectroDB schema into a model definition,
    /// resolving named callbacks through the registry.
    pub fn definition(&self) -> Result<Definition, String> {
        let s = &self.schema;
        let mut def = Definition {
            entity: s["model"]["entity"].as_str().unwrap_or_default().into(),
            version: s["model"]["version"].as_str().unwrap_or_default().into(),
            service: s["model"]["service"].as_str().unwrap_or_default().into(),
            ..Default::default()
        };
        for (k, v) in s["attributes"].as_object().into_iter().flatten() {
            def.attributes
                .push(attribute_def(k, v).map_err(|e| format!("attribute {k:?}: {e}"))?);
        }
        for (k, v) in s["indexes"].as_object().into_iter().flatten() {
            def.indexes
                .push(index_def(k, v).map_err(|e| format!("index {k:?}: {e}"))?);
        }
        Ok(def)
    }

    /// Builds the entity's normalized model.
    pub fn model(&self) -> Result<Model, String> {
        Model::new(self.definition()?).map_err(|e| e.to_string())
    }
}

fn strings(j: &Json) -> Vec<String> {
    j.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn attribute_def(name: &str, data: &Json) -> Result<AttributeDef, String> {
    let mut a = AttributeDef {
        name: name.into(),
        ..Default::default()
    };
    // Shorthand: "attr": "string" or "attr": ["a", "b"].
    let data = if data.is_object() {
        data.clone()
    } else {
        serde_json::json!({ "type": data })
    };
    let str_field = |k: &str| data[k].as_str().unwrap_or_default().to_string();
    a.field = str_field("field");
    a.label = data["label"].as_str().map(String::from);
    a.required = data["required"].as_bool().unwrap_or(false);
    a.hidden = data["hidden"].as_bool().unwrap_or(false);
    a.read_only = data["readOnly"].as_bool().unwrap_or(false);
    a.cast = str_field("cast");
    if data["padding"].is_object() {
        a.padding = Some(Padding {
            length: data["padding"]["length"].as_i64().unwrap_or(0),
            char: data["padding"]["char"].as_str().unwrap_or_default().into(),
        });
    }
    match &data["type"] {
        Json::Array(values) => {
            a.ty = AttrType::Enum;
            a.enum_values = values
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        Json::String(t) => {
            a.ty = AttrType::parse(t).ok_or_else(|| format!("invalid type {t:?}"))?
        }
        _ => {}
    }
    if !data["default"].is_null() {
        a.default = Some(default_fn(&data["default"])?);
    }
    if !data["get"].is_null() {
        a.get = Some(Fn::from_json(&data["get"])?.transform()?);
    }
    if !data["set"].is_null() {
        a.set = Some(Fn::from_json(&data["set"])?.transform()?);
    }
    if !data["validate"].is_null() {
        let f = Fn::from_json(&data["validate"])?;
        if f.name == "regexp" {
            a.validate_regex = Some(f.regexp()?);
        } else {
            a.validate = Some(f.validator()?);
        }
    }
    match &data["watch"] {
        Json::String(s) if s == "*" => a.watch_all = true,
        Json::String(_) => return Err("watch must be an array or \"*\"".into()),
        Json::Array(_) => a.watch = strings(&data["watch"]),
        _ => {}
    }
    if !data["items"].is_null() {
        a.items = Some(Box::new(
            attribute_def("", &data["items"]).map_err(|e| format!("items: {e}"))?,
        ));
    }
    if let Some(props) = data["properties"].as_object() {
        let mut out = Vec::new();
        for (k, v) in props {
            out.push(attribute_def(k, v).map_err(|e| format!("{k}: {e}"))?);
        }
        a.properties = Some(out);
    }
    Ok(a)
}

/// Resolves a default: a `$fn`, or a constant (decoded fresh on each call).
fn default_fn(data: &Json) -> Result<DefaultFn, String> {
    if data.get("$fn").is_some() {
        return Fn::from_json(data)?.default();
    }
    let data = data.clone();
    Ok(Arc::new(move || Ok(value(&data))))
}

fn key_def(j: &Json) -> KeyDef {
    KeyDef {
        field: j["field"].as_str().unwrap_or_default().into(),
        composite: j["composite"].as_array().map(|_| strings(&j["composite"])),
        template: j["template"].as_str().map(String::from),
        casing: j["casing"].as_str().unwrap_or_default().into(),
        cast: j["cast"].as_str().unwrap_or_default().into(),
    }
}

fn index_def(pattern: &str, data: &Json) -> Result<IndexDef, String> {
    let mut ix = IndexDef {
        access_pattern: pattern.into(),
        index: data["index"].as_str().unwrap_or_default().into(),
        scope: data["scope"].as_str().unwrap_or_default().into(),
        pk: key_def(&data["pk"]),
        sk: if data["sk"].is_object() {
            Some(key_def(&data["sk"]))
        } else {
            None
        },
        ..Default::default()
    };
    let t = data["type"].as_str().unwrap_or_default();
    ix.ty = IndexType::parse(t).ok_or_else(|| format!("invalid index type {t:?}"))?;
    match &data["collection"] {
        Json::Array(_) => {
            ix.collection_array = true;
            ix.collection = strings(&data["collection"]);
        }
        Json::String(s) => ix.collection = vec![s.clone()],
        _ => {}
    }
    if !data["condition"].is_null() {
        ix.condition = Some(Fn::from_json(&data["condition"])?.condition()?);
    }
    match &data["projection"] {
        Json::Array(_) => {
            ix.projection = Some(Projection {
                kind: String::new(),
                attributes: strings(&data["projection"]),
            })
        }
        Json::String(s) => {
            ix.projection = Some(Projection {
                kind: s.clone(),
                attributes: vec![],
            })
        }
        _ => {}
    }
    Ok(ix)
}

/// A named callback, `{"$fn": "<name>", "args": [...]}`. Its semantics
/// mirror `tools/recordvectors/registry.mjs` exactly.
#[derive(Clone, Debug)]
pub struct Fn {
    pub name: String,
    pub args: Vec<Json>,
}

impl Fn {
    pub fn from_json(j: &Json) -> Result<Fn, String> {
        let name = j["$fn"]
            .as_str()
            .ok_or_else(|| format!("expected a $fn: {j}"))?
            .to_string();
        Ok(Fn {
            name,
            args: j["args"].as_array().cloned().unwrap_or_default(),
        })
    }

    fn string_arg(&self, i: usize) -> Result<String, String> {
        self.args
            .get(i)
            .and_then(|a| a.as_str())
            .map(String::from)
            .ok_or_else(|| format!("$fn {:?}: missing string argument {i}", self.name))
    }

    /// Resolves an index condition callback.
    pub fn condition(&self) -> Result<ConditionFn, String> {
        match self.name.as_str() {
            "has" => {
                let attr = self.string_arg(0)?;
                Ok(Arc::new(move |a: &Attrs| Ok(a.contains_key(&attr))))
            }
            "equals" => {
                let attr = self.string_arg(0)?;
                let want = value(self.args.get(1).ok_or("$fn \"equals\": missing value")?);
                Ok(Arc::new(move |a: &Attrs| Ok(a.get(&attr) == Some(&want))))
            }
            n => Err(format!("$fn {n:?} is not a condition")),
        }
    }

    /// Resolves a default callback.
    pub fn default(&self) -> Result<DefaultFn, String> {
        match self.name.as_str() {
            "const" => default_fn(self.args.first().ok_or("$fn \"const\": missing value")?),
            n => Err(format!("$fn {n:?} is not a default")),
        }
    }

    /// Resolves a get or set callback.
    pub fn transform(&self) -> Result<TransformFn, String> {
        match self.name.as_str() {
            "upper" | "lower" => {
                let upper = self.name == "upper";
                Ok(Arc::new(move |v: Value, _: &Attrs| {
                    Ok(match v {
                        Value::String(s) => Value::String(if upper {
                            s.to_uppercase()
                        } else {
                            s.to_lowercase()
                        }),
                        v => v,
                    })
                }))
            }
            "prefix" => {
                let p = self.string_arg(0)?;
                Ok(Arc::new(move |v: Value, _: &Attrs| {
                    if v.is_undefined() {
                        return Ok(v);
                    }
                    Ok(Value::String(format!("{p}{}", js_string(&v))))
                }))
            }
            "join" => {
                let sep = self.string_arg(0)?;
                let attrs: Vec<String> = (1..self.args.len())
                    .map(|i| self.string_arg(i))
                    .collect::<Result<_, _>>()?;
                Ok(Arc::new(move |_: Value, item: &Attrs| {
                    let parts: Vec<String> = attrs
                        .iter()
                        .map(|a| match item.get(a) {
                            Some(v) if !matches!(v, Value::Null | Value::Undefined) => js_string(v),
                            _ => String::new(),
                        })
                        .collect();
                    Ok(Value::String(parts.join(&sep)))
                }))
            }
            n => Err(format!("$fn {n:?} is not a get/set transform")),
        }
    }

    /// Resolves a validate callback.
    pub fn validator(&self) -> Result<ValidateFn, String> {
        match self.name.as_str() {
            "minLength" => {
                let n = self
                    .args
                    .first()
                    .and_then(|a| a.as_u64())
                    .ok_or("$fn \"minLength\": missing length")? as usize;
                Ok(Arc::new(move |v: &Value| {
                    Ok(v.is_undefined() || js_length(&js_string(v)) >= n)
                }))
            }
            "throws" => {
                let msg = self.string_arg(0)?;
                Ok(Arc::new(move |_: &Value| Err(msg.clone().into())))
            }
            n => Err(format!("$fn {n:?} is not a validator")),
        }
    }

    /// Resolves `{"$fn": "regexp", "args": [source, flags]}`.
    pub fn regexp(&self) -> Result<regex::Regex, String> {
        let src = self.string_arg(0)?;
        let flags = self
            .args
            .get(1)
            .and_then(|a| a.as_str())
            .unwrap_or_default();
        RegexBuilder::new(&src)
            .case_insensitive(flags.contains('i'))
            .build()
            .map_err(|e| e.to_string())
    }
}
