//! Helpers shared by the vector tests: replaying recorded chains on the
//! engine and comparing what it builds with what ElectroDB recorded.

#![allow(dead_code)]

use base64::Engine as _;
use serde_json::{Value as Json, json};
use wattdb_engine::keys::QueryType;
use wattdb_engine::pager::{RunOptions, Runner};
use wattdb_engine::query::{Query, QueryOptions};
use wattdb_engine::request::Params;
use wattdb_engine::write::{Cond, DataOp, Entry, Options, Write};
use wattdb_engine::{Attrs, Error, Model, Path, wire};
use wattdb_vectors::{Case, Fn, Request, Step, Vector};

/// A vector case whose entity builds.
pub struct VectorCase {
    pub name: String,
    pub vector: String,
    pub table: String,
    pub entity: String,
    pub case: Case,
}

/// Every vector with its models, keyed by entity name.
pub fn vectors() -> Vec<(Vector, Vec<(String, Model)>)> {
    wattdb_vectors::load_all()
        .into_iter()
        .map(|v| {
            let models = v.models();
            (v, models)
        })
        .collect()
}

pub fn model<'a>(models: &'a [(String, Model)], name: &str) -> Option<&'a Model> {
    models.iter().find(|(n, _)| n == name).map(|(_, m)| m)
}

pub fn is_object(j: &Json) -> bool {
    j.is_object()
}

/// A JSON object's entries in document order.
pub fn entries_of(j: &Json) -> Vec<Entry> {
    let values = wattdb_vectors::attrs(j);
    wattdb_vectors::keys(j)
        .into_iter()
        .map(|k| Entry::new(k.clone(), values[&k].clone()))
        .collect()
}

/// Decodes `{"$fn": name, "args": [...]}`.
pub fn fn_args(j: &Json, want: &str) -> Vec<Json> {
    let f = Fn::from_json(j).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(f.name, want, "expected $fn {want:?}, got {j}");
    f.args
}

pub fn data_ops(j: &Json) -> Vec<DataOp> {
    fn_args(j, "data")
        .iter()
        .map(|st| {
            let parts = st.as_array().expect("data statement");
            DataOp {
                op: parts[0].as_str().unwrap().into(),
                path: Path::parse(parts[1].as_str().unwrap()),
                values: parts[2..].iter().map(wattdb_vectors::value).collect(),
            }
        })
        .collect()
}

pub fn where_cond(j: &Json) -> Cond {
    cond_of(&fn_args(j, "where")[0])
}

pub fn cond_of(j: &Json) -> Cond {
    let parts = j.as_array().expect("where expression");
    let head = parts[0].as_str().unwrap();
    match head {
        "and" => Cond::And(parts[1..].iter().map(cond_of).collect()),
        "or" => Cond::Or(parts[1..].iter().map(cond_of).collect()),
        "not" => Cond::Not(Box::new(cond_of(&parts[1]))),
        op => Cond::Op {
            op: op.into(),
            path: Path::parse(parts[1].as_str().unwrap()),
            values: parts[2..].iter().map(wattdb_vectors::value).collect(),
        },
    }
}

/// Replays a single-item chain on the engine builder.
pub fn build_write<'m>(m: &'m Model, chain: &[Step]) -> Write<'m> {
    let first = &chain[0];
    let attrs = wattdb_vectors::attrs(&first.args[0]);
    let mut w = match first.method.as_str() {
        "get" => Write::get(m, &attrs),
        "check" => Write::check(m, &attrs),
        "put" | "create" => Write::put(m, &attrs, first.method == "create"),
        "delete" | "remove" => Write::delete(m, &attrs, first.method == "remove"),
        "update" | "patch" => Write::update(m, &attrs, first.method == "patch"),
        "upsert" => Write::upsert(m, &attrs, &wattdb_vectors::keys(&first.args[0])),
        other => panic!("unsupported first step {other:?}"),
    };
    for step in &chain[1..] {
        match step.method.as_str() {
            "set" | "add" | "subtract" | "append" | "delete" | "ifNotExists" => {
                w.apply(&step.method, &entries_of(&step.args[0]));
            }
            "composite" => {
                w.composite(&entries_of(&step.args[0]));
            }
            "remove" => {
                let paths: Vec<String> = serde_json::from_value(step.args[0].clone()).unwrap();
                w.remove(&paths);
            }
            "data" => {
                w.data(&data_ops(&step.args[0]));
            }
            "where" => {
                w.where_(&where_cond(&step.args[0]));
            }
            other => panic!("unsupported chain step {other:?}"),
        }
    }
    w
}

pub fn options_of(j: Option<&Json>, exec: bool) -> Options {
    let mut o = Options {
        exec,
        ..Default::default()
    };
    let Some(j) = j else { return o };
    let s = |k: &str| j[k].as_str().unwrap_or_default().to_string();
    o.table = s("table");
    o.response = s("response");
    o.consistent = j["consistent"].as_bool().unwrap_or(false);
    o.attributes = serde_json::from_value(j["attributes"].clone()).unwrap_or_default();
    o.include_keys = j["data"] == "includeKeys";
    match &j["returnOnConditionCheckFailure"] {
        Json::Bool(true) => o.return_rejected = "true".into(),
        Json::String(s) if s == "true" || s == "all_old" => o.return_rejected = s.clone(),
        _ => {}
    }
    o
}

pub struct QueryOpts {
    pub query: QueryOptions,
    pub run: RunOptions,
    pub cursor: String,
}

pub fn query_options_of(j: Option<&Json>, exec: bool) -> QueryOpts {
    let mut o = QueryOpts {
        query: QueryOptions {
            exec,
            ..Default::default()
        },
        run: RunOptions {
            pages: 1,
            ..Default::default()
        },
        cursor: String::new(),
    };
    let Some(j) = j else { return o };
    let s = |k: &str| j[k].as_str().unwrap_or_default().to_string();
    let q = &mut o.query;
    q.table = s("table");
    q.limit = j["limit"].as_i64().unwrap_or(0);
    q.order = s("order");
    q.consistent = j["consistent"].as_bool().unwrap_or(false);
    q.attributes = serde_json::from_value(j["attributes"].clone()).unwrap_or_default();
    q.ignore_ownership = j["ignoreOwnership"].as_bool().unwrap_or(false);
    q.compare = s("compare");
    q.raw = j["data"] == "raw";
    q.include_keys = j["data"] == "includeKeys";
    q.hydrate = j["hydrate"].as_bool().unwrap_or(false);
    o.run.count = j["count"].as_u64().unwrap_or(0) as usize;
    o.run.ignore_ownership = q.ignore_ownership;
    o.run.attributes = q.attributes.clone();
    o.cursor = s("cursor");
    match &j["pages"] {
        Json::String(p) if p == "all" => o.run.pages = 0,
        Json::String(p) => o.run.pages = p.parse().unwrap_or(1),
        Json::Number(n) => o.run.pages = n.as_u64().unwrap_or(1) as usize,
        _ => {}
    }
    o
}

pub fn build_query<'m>(m: &'m Model, case: &Case) -> Query<'m> {
    let first = &case.chain[0];
    let mut q = match first.method.as_str() {
        "scan" => Query::scan(m, ""),
        "find" => Query::find(m, entries_of(&first.args[0])),
        "match" => Query::match_(m, entries_of(&first.args[0])),
        method => match method.strip_prefix("scan.") {
            Some(pattern) => Query::scan(m, pattern),
            None => Query::new(
                m,
                method.strip_prefix("query.").unwrap(),
                entries_of(&first.args[0]),
            ),
        },
    };
    apply_steps(&mut q, &case.chain[1..]);
    q
}

pub fn apply_steps(q: &mut Query<'_>, steps: &[Step]) {
    for step in steps {
        match step.method.as_str() {
            "begins" => {
                q.begins(&entries_of(&step.args[0]));
            }
            "between" => {
                q.between(&entries_of(&step.args[0]), &entries_of(&step.args[1]));
            }
            "where" => {
                q.where_(&where_cond(&step.args[0]));
            }
            op => {
                let typ = match op {
                    "gt" => QueryType::Gt,
                    "gte" => QueryType::Gte,
                    "lt" => QueryType::Lt,
                    "lte" => QueryType::Lte,
                    other => panic!("unsupported step {other:?}"),
                };
                q.compare(typ, &entries_of(&step.args[0]));
            }
        }
    }
}

/// A Runner answering from recorded responses and logging requests in wire
/// form.
pub struct Scripted {
    pub responses: Vec<Json>,
    pub requests: Vec<Json>,
}

impl Scripted {
    pub fn new(responses: &[Json]) -> Self {
        Scripted {
            responses: responses.to_vec(),
            requests: Vec::new(),
        }
    }

    fn next(&mut self) -> Json {
        if self.responses.is_empty() {
            json!({})
        } else {
            self.responses.remove(0)
        }
    }
}

impl Runner for Scripted {
    type Error = Error;

    async fn page(&mut self, params: Params) -> Result<(Vec<Attrs>, Option<Attrs>), Error> {
        self.requests.push(params.to_json());
        let body = self.next();
        let items = body["Items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(wattdb_vectors::attribute_map)
            .collect();
        let last = body
            .get("LastEvaluatedKey")
            .map(wattdb_vectors::attribute_map);
        Ok((items, last))
    }

    async fn batch_get(&mut self, table: &str, keys: Vec<Attrs>) -> Result<Vec<Attrs>, Error> {
        let ks: Vec<Json> = keys.iter().map(wire::attrs_to_json).collect();
        self.requests
            .push(json!({ "RequestItems": { table: { "Keys": ks } } }));
        let body = self.next();
        Ok(body["Responses"][table]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(wattdb_vectors::attribute_map)
            .collect())
    }
}

/// Compares sent requests with recorded ones, as JSON.
pub fn check_requests(name: &str, got: &[Json], want: &[Request]) -> Result<(), String> {
    if got.len() != want.len() {
        return Err(format!(
            "{name}: sent {} requests, ElectroDB sent {}\n got: {got:#?}",
            got.len(),
            want.len()
        ));
    }
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if *g != w.request {
            return Err(format!(
                "{name}: request {i} differs\n got: {}\nwant: {}",
                serde_json::to_string_pretty(g).unwrap(),
                serde_json::to_string_pretty(&w.request).unwrap()
            ));
        }
    }
    Ok(())
}

/// The JSON a cursor encodes.
pub fn decode_cursor_json(c: &str) -> Json {
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c.trim_end_matches('='))
        .unwrap_or_default();
    serde_json::from_slice(&b).unwrap_or(Json::Null)
}

/// Collects failures so every case runs, then reports them together.
#[derive(Default)]
pub struct Failures(pub Vec<String>);

impl Failures {
    pub fn check(&mut self, r: Result<(), String>) {
        if let Err(e) = r {
            self.0.push(e);
        }
    }

    pub fn finish(self, what: &str, checked: usize) {
        assert!(checked > 0, "no {what} cases were checked");
        if !self.0.is_empty() {
            panic!(
                "{} of {checked} {what} cases failed:\n\n{}",
                self.0.len(),
                self.0.join("\n\n")
            );
        }
        eprintln!("checked {checked} {what} cases");
    }
}
