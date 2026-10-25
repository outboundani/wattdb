//! Every recorded single-item case, driven through the engine: the whole
//! request (placeholders included) and the formatted result.

mod common;

use common::*;
use serde_json::{Value as Json, json};
use wattdb_engine::write::{Response, Returned};
use wattdb_engine::{Model, wire};
use wattdb_vectors::Case;

const WRITE_METHODS: [&str; 8] = [
    "get", "put", "create", "delete", "remove", "update", "patch", "upsert",
];

fn marshal_error(msg: &str) -> bool {
    msg.contains("MAX_SAFE_INTEGER") || msg.contains("non-empty set")
}

fn check_params(m: &Model, table: &str, c: &Case) -> Result<(), String> {
    if c.params.is_none() && c.params_error.is_none() {
        return Ok(());
    }
    let req = build_write(m, &c.chain).build(table, options_of(c.params_options.as_ref(), false));
    if let Some(e) = &c.params_error {
        if marshal_error(&e.message) {
            return Ok(()); // DocumentClient marshalling (D7, D18)
        }
        return match req {
            Ok(r) => Err(format!(
                "ElectroDB failed ({}) but wattdb built {}",
                e.message,
                r.params.to_json()
            )),
            Err(_) => Ok(()),
        };
    }
    let req = req.map_err(|e| format!("build: {e}"))?;
    let want = &c.params.as_ref().unwrap()[0].request;
    let got = req.params.to_json();
    if got != *want {
        return Err(format!(
            "params differ\n got: {}\nwant: {}",
            serde_json::to_string_pretty(&got).unwrap(),
            serde_json::to_string_pretty(want).unwrap()
        ));
    }
    Ok(())
}

fn check_go(m: &Model, table: &str, c: &Case) -> Result<(), String> {
    let Some(go) = &c.go else { return Ok(()) };
    let req = build_write(m, &c.chain).build(table, options_of(c.go_options.as_ref(), true));
    if go.error.is_some() && go.requests.is_empty() {
        let msg = &go.error.as_ref().unwrap().message;
        if !marshal_error(msg) && req.is_ok() {
            return Err(format!(
                "ElectroDB failed ({msg}) but wattdb built a request"
            ));
        }
        return Ok(());
    }
    let req = req.map_err(|e| format!("build: {e}"))?;
    let got = req.params.to_json();
    if got != go.requests[0].request {
        return Err(format!(
            "go request differs\n got: {}\nwant: {}",
            serde_json::to_string_pretty(&got).unwrap(),
            serde_json::to_string_pretty(&go.requests[0].request).unwrap()
        ));
    }
    if go.error.is_some() {
        return Ok(()); // AWS errors without returnOnConditionCheckFailure
    }
    let body = c.responses.first().cloned().unwrap_or(json!({}));
    let resp: Response = if body.get("__type").is_some() {
        let item = body.get("Item").map(wattdb_vectors::attribute_map);
        req.format_rejected(item.as_ref())
    } else {
        req.format(&Returned {
            item: body.get("Item").map(wattdb_vectors::attribute_map),
            attributes: body.get("Attributes").map(wattdb_vectors::attribute_map),
        })
    }
    .map_err(|e| format!("format: {e}"))?;
    let mut got = serde_json::Map::new();
    if resp.rejected {
        got.insert("rejected".into(), json!({ "BOOL": true }));
    }
    if let Some(data) = &resp.data {
        got.insert("data".into(), json!({ "M": wire::attrs_to_json(data) }));
    } else if resp.null {
        got.insert("data".into(), json!({ "NULL": true }));
    }
    let got = json!({ "M": Json::Object(got) });
    if Some(&got) != go.result.as_ref() {
        return Err(format!(
            "result differs\n got: {got}\nwant: {}",
            go.result.clone().unwrap_or_default()
        ));
    }
    Ok(())
}

#[test]
fn writes_match_vectors() {
    let mut failures = Failures::default();
    let mut checked = 0;
    for (v, models) in vectors() {
        for c in &v.cases {
            let Some(m) = model(&models, &c.entity) else {
                continue;
            };
            let Some(first) = c.chain.first() else {
                continue;
            };
            if !WRITE_METHODS.contains(&first.method.as_str())
                || first.args.is_empty()
                || !is_object(&first.args[0])
            {
                continue; // batch forms
            }
            let name = format!("{}/{}", v.name, c.name);
            checked += 1;
            failures.check(check_params(m, &v.table, c).map_err(|e| format!("{name}: {e}")));
            failures.check(check_go(m, &v.table, c).map_err(|e| format!("{name}: {e}")));
        }
    }
    failures.finish("single-item", checked);
}
