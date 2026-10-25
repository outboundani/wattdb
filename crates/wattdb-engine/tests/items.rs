//! Whole put/create items, rejections, and read formatting.

mod common;

use common::*;
use wattdb_engine::item::{FormatOptions, format_item, owns_item, put_item};
use wattdb_engine::{Value, wire};

#[test]
fn put_items_match_vectors() {
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
            if (first.method != "put" && first.method != "create")
                || first.args.is_empty()
                || !is_object(&first.args[0])
            {
                continue;
            }
            let name = format!("{}/{}", v.name, c.name);
            let payload = wattdb_vectors::attrs(&first.args[0]);
            let item =
                put_item(m, &payload).and_then(|item| wire::check_attrs(&item).map(|_| item));
            let js_err = c.params_error.as_ref().or(c.error.as_ref());
            let r = match (js_err, item) {
                (Some(e), _) if e.message.contains("MAX_SAFE_INTEGER") => continue, // D7: no guard on unsafe numbers
                (Some(e), Ok(item)) => Err(format!(
                    "{name}: ElectroDB failed ({}) but wattdb wrote {}",
                    e.message,
                    wire::attrs_to_json(&item)
                )),
                (Some(_), Err(_)) => Ok(()),
                (None, Err(e)) => Err(format!("{name}: put_item: {e}")),
                (None, Ok(item)) => {
                    let Some(req) = c.requests().first() else {
                        continue;
                    };
                    let got = wire::attrs_to_json(&item);
                    if got != req.request["Item"] {
                        Err(format!(
                            "{name}: items differ\n got: {got}\nwant: {}",
                            req.request["Item"]
                        ))
                    } else {
                        Ok(())
                    }
                }
            };
            checked += 1;
            failures.check(r);
        }
    }
    failures.finish("put", checked);
}

#[test]
fn read_formatting_matches_vectors() {
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
            let Some(result) = c.go.as_ref().and_then(|g| g.result.as_ref()) else {
                continue;
            };
            if c.responses.is_empty() || first.method != "get" || !is_object(&first.args[0]) {
                continue;
            }
            let name = format!("{}/{}", v.name, c.name);
            let opts = c.go_options.clone().unwrap_or_default();
            let format = FormatOptions {
                include_keys: opts["data"] == "includeKeys",
                attributes: serde_json::from_value(opts["attributes"].clone()).unwrap_or_default(),
            };
            let ignore = opts["ignoreOwnership"].as_bool().unwrap_or(false);
            let want = wattdb_vectors::attribute_value(&result["M"]["data"]);
            let mut formatted = Vec::new();
            for resp in &c.responses {
                let mut items: Vec<serde_json::Value> =
                    resp["Items"].as_array().cloned().unwrap_or_default();
                if resp.get("Item").is_some() {
                    items.push(resp["Item"].clone());
                }
                for raw in items {
                    let item = wattdb_vectors::attribute_map(&raw);
                    if !ignore && !owns_item(m, &item) {
                        continue;
                    }
                    formatted.push(format_item(m, &item, &format).unwrap());
                }
            }
            let got = formatted
                .into_iter()
                .next()
                .map(Value::Map)
                .unwrap_or(Value::Null);
            checked += 1;
            failures.check(if got != want {
                Err(format!(
                    "{name}: result differs\n got: {}\nwant: {}",
                    wire::to_json(&got),
                    wire::to_json(&want)
                ))
            } else {
                Ok(())
            });
        }
    }
    failures.finish("read", checked);
}
