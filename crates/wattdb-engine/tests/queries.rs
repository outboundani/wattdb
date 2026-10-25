//! Recorded queries and scans: params, every request sent (pages, cursors,
//! hydrate), and the results.

mod common;

use common::*;
use wattdb_engine::pager::encode_cursor;
use wattdb_engine::{Model, Value, wire};
use wattdb_vectors::Case;

fn check_params(m: &Model, table: &str, c: &Case) -> Result<(), String> {
    let o = query_options_of(c.params_options.as_ref(), false);
    let r = build_query(m, c).build(table, o.query);
    if let Some(e) = &c.params_error {
        return match r {
            Ok(r) => Err(format!(
                "ElectroDB failed ({}) but wattdb built {}",
                e.message,
                r.params.to_json()
            )),
            Err(_) => Ok(()),
        };
    }
    let r = r.map_err(|e| format!("build: {e}"))?;
    let Some(want) = c.params.as_ref().and_then(|p| p.first()) else {
        return Ok(());
    };
    let got = r.params.to_json();
    if got != want.request {
        return Err(format!(
            "params differ\n got: {}\nwant: {}",
            serde_json::to_string_pretty(&got).unwrap(),
            serde_json::to_string_pretty(&want.request).unwrap()
        ));
    }
    Ok(())
}

async fn check_go(name: &str, m: &Model, table: &str, c: &Case) -> Result<(), String> {
    let go = c.go.as_ref().unwrap();
    let o = query_options_of(c.go_options.as_ref(), true);
    let raw = o.query.raw;
    let r = build_query(m, c).build(table, o.query);
    if let Some(e) = &go.error {
        return match r {
            Ok(_) => Err(format!(
                "ElectroDB failed ({}) but wattdb built a request",
                e.message
            )),
            Err(_) => Ok(()),
        };
    }
    let r = r.map_err(|e| format!("build: {e}"))?;
    let start = r.start_key(&o.cursor).map_err(|e| e.to_string())?;
    let mut run = Scripted::new(&c.responses);
    if raw {
        // raw returns the first page as is
        wattdb_engine::pager::Runner::page(&mut run, r.with_start_key(start.as_ref()))
            .await
            .unwrap();
        return check_requests(name, &run.requests, &go.requests);
    }
    let res = r
        .run(start, &o.run, &mut run)
        .await
        .map_err(|e| format!("run: {e}"))?;
    check_requests(name, &run.requests, &go.requests)?;

    let result = go.result.as_ref().unwrap();
    let want = &result["M"];
    let got_data = Value::List(res.items.iter().cloned().map(Value::Map).collect());
    let want_data = wattdb_vectors::attribute_value(&want["data"]);
    if got_data != want_data {
        return Err(format!(
            "data differs\n got: {}\nwant: {}",
            wire::to_json(&got_data),
            want["data"]
        ));
    }
    let got_cursor = encode_cursor(&res.next.clone().unwrap_or_default()).unwrap();
    match (want["cursor"]["S"].as_str(), got_cursor) {
        (None, Some(g)) => return Err(format!("cursor = {}, want none", decode_cursor_json(&g))),
        (Some(w), g)
            if decode_cursor_json(&g.clone().unwrap_or_default()) != decode_cursor_json(w) =>
        {
            return Err(format!(
                "cursor = {}, want {}",
                decode_cursor_json(&g.unwrap_or_default()),
                decode_cursor_json(w)
            ));
        }
        _ => {}
    }
    if want.get("unprocessed").is_some() {
        let got = Value::List(res.unprocessed.iter().cloned().map(Value::Map).collect());
        if got != wattdb_vectors::attribute_value(&want["unprocessed"]) {
            return Err(format!("unprocessed differs: got {}", wire::to_json(&got)));
        }
    }
    Ok(())
}

#[tokio::test]
async fn queries_match_vectors() {
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
            let method = first.method.as_str();
            if !method.starts_with("query.")
                && method != "scan"
                && !method.starts_with("scan.")
                && method != "find"
                && method != "match"
            {
                continue;
            }
            let name = format!("{}/{}", v.name, c.name);
            checked += 1;
            if c.params.is_some() || c.params_error.is_some() {
                failures.check(check_params(m, &v.table, c).map_err(|e| format!("{name}: {e}")));
            }
            if c.go.is_some() {
                failures.check(
                    check_go(&name, m, &v.table, c)
                        .await
                        .map_err(|e| format!("{name}: {e}")),
                );
            }
        }
    }
    failures.finish("query", checked);
}
