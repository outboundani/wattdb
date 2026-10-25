//! Recorded write and get transactions: params, and results for committed,
//! canceled and get transactions.

mod common;

use common::*;
use serde_json::{Value as Json, json};
use wattdb_engine::tx::{TxResult, tx_params};
use wattdb_engine::write::Request;
use wattdb_engine::{Attrs, Model, Value, wire};
use wattdb_vectors::Case;

fn token(j: Option<&Json>) -> Option<String> {
    j.and_then(|o| o["token"].as_str()).map(String::from)
}

fn check_tx(name: &str, models: &[(String, Model)], table: &str, c: &Case) -> Result<(), String> {
    let write = c.chain[0].method == "transaction.write";
    let ops = c.chain[0].args[0]["args"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let build = |exec: bool| -> Vec<Request<'_>> {
        ops.iter()
            .map(|op| {
                let chain = wattdb_vectors::parse_chain(&op["chain"]);
                let mut opt = options_of(
                    if op["commit"].is_object() {
                        Some(&op["commit"])
                    } else {
                        None
                    },
                    exec,
                );
                opt.transaction = true;
                let m = model(models, op["entity"].as_str().unwrap()).unwrap();
                build_write(m, &chain).build(table, opt).unwrap()
            })
            .collect()
    };
    if let Some(p) = &c.params {
        let reqs = build(false);
        check_requests(
            name,
            &[tx_params(&reqs, token(c.params_options.as_ref()).as_deref()).to_json()],
            p,
        )?;
    }
    let Some(go) = &c.go else { return Ok(()) };
    let reqs = build(true);
    let tok = token(c.go_options.as_ref());
    let got = tx_params(&reqs, tok.as_deref()).to_json();
    if !write || tok.is_some() {
        check_requests(name, &[got], &go.requests)?;
    } else {
        // ElectroDB sends a random token when none is given.
        let mut want = go.requests[0].request.clone();
        want.as_object_mut().unwrap().remove("ClientRequestToken");
        if got != want {
            return Err(format!(
                "{name}: request differs\n got: {got}\nwant: {want}"
            ));
        }
    }

    let resp = c.responses.first().cloned().unwrap_or(json!({}));
    let canceled = resp["__type"]
        .as_str()
        .is_some_and(|t| t.ends_with("TransactionCanceledException"));
    let mut results = Vec::new();
    for (i, r) in reqs.iter().enumerate() {
        let res: TxResult = if canceled {
            let reason = &resp["CancellationReasons"][i];
            let item = reason
                .get("Item")
                .filter(|j| !j.is_null())
                .map(wattdb_vectors::attribute_map);
            r.tx_canceled_result(
                reason["Code"].as_str().unwrap_or_default(),
                reason["Message"].as_str().unwrap_or_default(),
                item.as_ref(),
            )
            .map_err(|e| e.to_string())?
        } else if write {
            TxResult::write()
        } else {
            let item = resp["Responses"][i]
                .get("Item")
                .filter(|j| !j.is_null())
                .map(wattdb_vectors::attribute_map);
            r.tx_get_result(item.as_ref()).map_err(|e| e.to_string())?
        };
        results.push(res);
    }
    let data: Vec<Value> = results
        .iter()
        .map(|r| {
            let mut m = Attrs::new();
            m.insert("rejected".into(), Value::Bool(r.rejected));
            m.insert(
                "item".into(),
                r.item.clone().map(Value::Map).unwrap_or(Value::Null),
            );
            if write {
                m.insert("code".into(), Value::String(r.code.clone()));
                if !r.message.is_empty() {
                    m.insert("message".into(), Value::String(r.message.clone()));
                }
            }
            Value::Map(m)
        })
        .collect();
    let mut got = Attrs::new();
    got.insert("data".into(), Value::List(data));
    got.insert("canceled".into(), Value::Bool(canceled));
    let got = Value::Map(got);
    let want = wattdb_vectors::attribute_value(go.result.as_ref().unwrap());
    if got != want {
        return Err(format!(
            "{name}: result differs\n got: {}\nwant: {}",
            wire::to_json(&got),
            wire::to_json(&want)
        ));
    }
    Ok(())
}

#[test]
fn transactions_match_vectors() {
    let mut failures = Failures::default();
    let mut checked = 0;
    for (v, models) in vectors() {
        for c in &v.cases {
            if !c
                .chain
                .first()
                .is_some_and(|s| s.method.starts_with("transaction."))
            {
                continue;
            }
            let name = format!("{}/{}", v.name, c.name);
            checked += 1;
            failures.check(check_tx(&name, &models, &v.table, c));
        }
    }
    failures.finish("transaction", checked);
}
