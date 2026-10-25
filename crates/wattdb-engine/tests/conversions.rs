//! Conversions and parse against their recorded values and errors.

mod common;

use common::*;
use serde_json::Value as Json;
use wattdb_engine::conv::{self, ParseInput, ParseOutput, Strict};
use wattdb_engine::item::FormatOptions;
use wattdb_engine::{Attrs, Model, Value, wire};
use wattdb_vectors::Step;

enum Out {
    Value(Value),
    Cursor(Option<String>),
}

fn run_conversion(m: &Model, step: &Step) -> Result<Out, String> {
    let e = |e: wattdb_engine::Error| e.to_string();
    if step.method == "parse" {
        let resp = &step.args[0];
        let opts = step.args.get(1).cloned().unwrap_or(Json::Null);
        let fo = FormatOptions {
            include_keys: opts["data"] == "includeKeys",
            attributes: serde_json::from_value(opts["attributes"].clone()).unwrap_or_default(),
        };
        let input = if resp.get("Item").is_some() {
            ParseInput::Item(wattdb_vectors::attrs(&resp["Item"]))
        } else if resp.get("Items").is_some() {
            ParseInput::Items(
                resp["Items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(wattdb_vectors::attrs)
                    .collect(),
            )
        } else {
            ParseInput::Attributes(wattdb_vectors::attrs(&resp["Attributes"]))
        };
        let data = match conv::parse(m, &input, &fo, true).map_err(e)? {
            ParseOutput::Item(i) => i.map(Value::Map).unwrap_or(Value::Null),
            ParseOutput::Items(items) => Value::List(items.into_iter().map(Value::Map).collect()),
        };
        let mut out = Attrs::new();
        out.insert("data".into(), data);
        return Ok(Out::Value(Value::Map(out)));
    }
    let path: Vec<&str> = step
        .method
        .strip_prefix("conversions.")
        .unwrap()
        .split('.')
        .collect();
    let strict = match step.args.get(1).and_then(|o| o["strict"].as_str()) {
        Some("pk") => Strict::Pk,
        Some("all") => Strict::All,
        _ => Strict::None,
    };
    let arg = &step.args[0];
    let cursor = || arg.as_str().unwrap_or_default().to_string();
    let attrs = || wattdb_vectors::attrs(arg);
    let map = |a: Attrs| Out::Value(Value::Map(a));
    if path[0] == "byAccessPattern" {
        let ix = m
            .access_pattern(path[1])
            .ok_or_else(|| format!("unknown access pattern {}", path[1]))?;
        return Ok(match path[2..].join(".").as_str() {
            "fromComposite.toKeys" => {
                map(conv::composite_to_keys_by_index(m, ix, &attrs(), strict)
                    .map_err(e)?
                    .unwrap_or_default())
            }
            "fromComposite.toCursor" => {
                Out::Cursor(conv::composite_to_cursor_by_index(m, ix, &attrs()).map_err(e)?)
            }
            "fromKeys.toCursor" => {
                Out::Cursor(conv::keys_to_cursor_by_index(m, ix, &attrs()).map_err(e)?)
            }
            "fromKeys.toComposite" => {
                map(conv::keys_to_composite_by_index(m, ix, &attrs()).map_err(e)?)
            }
            "fromCursor.toKeys" => map(conv::cursor_to_keys(&cursor()).map_err(e)?),
            "fromCursor.toComposite" => {
                map(conv::cursor_to_composite_by_index(m, ix, &cursor()).map_err(e)?)
            }
            other => panic!("unsupported conversion {other}"),
        });
    }
    Ok(match path.join(".").as_str() {
        "fromComposite.toKeys" => map(conv::composite_to_keys(m, &attrs(), strict).map_err(e)?),
        "fromComposite.toCursor" => Out::Cursor(conv::composite_to_cursor(m, &attrs()).map_err(e)?),
        "fromKeys.toCursor" => Out::Cursor(conv::keys_to_cursor(&attrs()).map_err(e)?),
        "fromKeys.toComposite" => map(conv::keys_to_composite(m, &attrs()).map_err(e)?),
        "fromCursor.toKeys" => map(conv::cursor_to_keys(&cursor()).map_err(e)?),
        "fromCursor.toComposite" => map(conv::cursor_to_composite(m, &cursor()).map_err(e)?),
        other => panic!("unsupported conversion {other}"),
    })
}

#[test]
fn conversions_match_vectors() {
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
            if !first.method.starts_with("conversions.") && first.method != "parse" {
                continue;
            }
            let name = format!("{}/{}", v.name, c.name);
            checked += 1;
            let got = run_conversion(m, first);
            let r = match (&c.error, got) {
                (Some(e), Ok(_)) => Err(format!(
                    "{name}: ElectroDB failed ({}) but wattdb returned a value",
                    e.message
                )),
                (Some(_), Err(_)) => Ok(()),
                (None, Err(e)) => Err(format!("{name}: {e}")),
                (None, Ok(Out::Cursor(got))) => {
                    let want = c
                        .value
                        .as_ref()
                        .and_then(|v| v["S"].as_str())
                        .unwrap_or_default();
                    let got = got.unwrap_or_default();
                    if decode_cursor_json(&got) != decode_cursor_json(want) {
                        Err(format!(
                            "{name}: cursor = {}, want {}",
                            decode_cursor_json(&got),
                            decode_cursor_json(want)
                        ))
                    } else {
                        Ok(())
                    }
                }
                (None, Ok(Out::Value(got))) => {
                    let want = wattdb_vectors::attribute_value(c.value.as_ref().unwrap());
                    if got != want {
                        Err(format!(
                            "{name}: value differs\n got: {}\nwant: {}",
                            wire::to_json(&got),
                            wire::to_json(&want)
                        ))
                    } else {
                        Ok(())
                    }
                }
            };
            failures.check(r);
        }
    }
    failures.finish("conversion", checked);
}
