//! Recorded batch gets, puts and deletes: the chunked requests, and the
//! formatted results and unprocessed items.

mod common;

use common::*;
use serde_json::{Value as Json, json};
use wattdb_engine::batch::{BatchGet, BatchGetOptions, BatchWrite, BatchWriteOptions};
use wattdb_engine::request::WriteRequest;
use wattdb_engine::{Attrs, Value, wire};

fn opt_value(v: &Option<Attrs>) -> Value {
    v.clone().map(Value::Map).unwrap_or(Value::Null)
}

#[test]
fn batches_match_vectors() {
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
            if first.args.is_empty()
                || is_object(&first.args[0])
                || !matches!(first.method.as_str(), "get" | "put" | "delete")
            {
                continue;
            }
            let name = format!("{}/{}", v.name, c.name);
            let args: Vec<Attrs> = first.args[0]
                .as_array()
                .unwrap()
                .iter()
                .map(wattdb_vectors::attrs)
                .collect();
            let opts = c.go_options.clone().unwrap_or_default();
            let want_err =
                c.params_error.is_some() || c.go.as_ref().is_some_and(|g| g.error.is_some());
            let responses: Vec<Json> = c.responses.clone();

            let outcome: Result<(Vec<Json>, Json), String> = if first.method == "get" {
                match BatchGet::new(
                    m,
                    &v.table,
                    &args,
                    BatchGetOptions {
                        attributes: serde_json::from_value(opts["attributes"].clone())
                            .unwrap_or_default(),
                        ignore_ownership: opts["ignoreOwnership"].as_bool().unwrap_or(false),
                        preserve_order: opts["preserveBatchOrder"].as_bool().unwrap_or(false),
                        exec: true,
                        ..Default::default()
                    },
                ) {
                    Err(e) => Err(e.to_string()),
                    Ok(mut b) => {
                        let reqs: Vec<Json> = b.requests.iter().map(|r| r.to_json()).collect();
                        for i in 0..b.requests.len() {
                            let body = responses.get(i).cloned().unwrap_or(json!({}));
                            let items: Vec<Attrs> = body["Responses"][&v.table]
                                .as_array()
                                .cloned()
                                .unwrap_or_default()
                                .iter()
                                .map(wattdb_vectors::attribute_map)
                                .collect();
                            let un: Vec<Attrs> = body["UnprocessedKeys"][&v.table]["Keys"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default()
                                .iter()
                                .map(wattdb_vectors::attribute_map)
                                .collect();
                            b.apply(&items, &un).unwrap();
                        }
                        let data = Value::List(b.data().iter().map(opt_value).collect());
                        let un = Value::List(b.unprocessed.iter().map(opt_value).collect());
                        let mut res = Attrs::new();
                        res.insert("data".into(), data);
                        res.insert("unprocessed".into(), un);
                        Ok((reqs, wire::to_json(&Value::Map(res))))
                    }
                }
            } else {
                let built = if first.method == "put" {
                    BatchWrite::put(m, &v.table, &args, BatchWriteOptions::default())
                } else {
                    BatchWrite::delete(m, &v.table, &args, BatchWriteOptions::default())
                };
                match built {
                    Err(e) => Err(e.to_string()),
                    Ok(mut b) => {
                        let reqs: Vec<Json> = b.requests.iter().map(|r| r.to_json()).collect();
                        for i in 0..b.requests.len() {
                            let body = responses.get(i).cloned().unwrap_or(json!({}));
                            let un: Vec<WriteRequest> = body["UnprocessedItems"][&v.table]
                                .as_array()
                                .cloned()
                                .unwrap_or_default()
                                .iter()
                                .map(|r| match r.get("PutRequest") {
                                    Some(p) => {
                                        WriteRequest::Put(wattdb_vectors::attribute_map(&p["Item"]))
                                    }
                                    None => WriteRequest::Delete(wattdb_vectors::attribute_map(
                                        &r["DeleteRequest"]["Key"],
                                    )),
                                })
                                .collect();
                            b.apply(&un).unwrap();
                        }
                        let mut res = Attrs::new();
                        res.insert(
                            "unprocessed".into(),
                            Value::List(b.unprocessed.iter().map(opt_value).collect()),
                        );
                        Ok((reqs, wire::to_json(&Value::Map(res))))
                    }
                }
            };
            checked += 1;
            let r = match outcome {
                Err(e) if !want_err => Err(format!("{name}: {e}")),
                Err(_) => Ok(()),
                Ok(_) if want_err => Err(format!(
                    "{name}: ElectroDB failed but wattdb built the batch"
                )),
                Ok((reqs, result)) => (|| {
                    if let Some(p) = &c.params {
                        check_requests(&name, &reqs, p)?;
                    }
                    let Some(go) = &c.go else { return Ok(()) };
                    check_requests(&name, &reqs, &go.requests)?;
                    let want = wire::to_json(&wattdb_vectors::attribute_value(
                        go.result.as_ref().unwrap(),
                    ));
                    if result != want {
                        return Err(format!(
                            "{name}: result differs\n got: {result}\nwant: {want}"
                        ));
                    }
                    Ok(())
                })(),
            };
            failures.check(r);
        }
    }
    failures.finish("batch", checked);
}
