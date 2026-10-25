//! Recorded collection queries: params, requests and the per-entity results.

mod common;

use common::*;
use wattdb_engine::pager::encode_cursor;
use wattdb_engine::query::{CollectionMember, Query};
use wattdb_engine::{Attrs, Value, wire};

#[tokio::test]
async fn collections_match_vectors() {
    let mut failures = Failures::default();
    let mut checked = 0;
    for (v, models) in vectors() {
        for c in &v.cases {
            let Some(name) = c
                .chain
                .first()
                .and_then(|s| s.method.strip_prefix("collections."))
            else {
                continue;
            };
            let aliases = v
                .services
                .iter()
                .find(|(s, _)| *s == c.service)
                .map(|(_, a)| a.clone())
                .unwrap_or_default();
            let members: Vec<CollectionMember<'_>> = aliases
                .iter()
                .filter_map(|alias| {
                    let m = model(&models, alias)?;
                    m.collection_index(name).map(|_| CollectionMember {
                        alias: alias.clone(),
                        model: m,
                    })
                })
                .collect();
            let case = format!("{}/{}", v.name, c.name);
            let build = || {
                let mut q =
                    Query::collection(name, members.clone(), entries_of(&c.chain[0].args[0]))
                        .unwrap();
                apply_steps(&mut q, &c.chain[1..]);
                q
            };
            checked += 1;
            let r: Result<(), String> = async {
                if let Some(p) = &c.params {
                    let o = query_options_of(c.params_options.as_ref(), false);
                    let r = build()
                        .build(&v.table, o.query)
                        .map_err(|e| e.to_string())?;
                    let got = r.params.to_json();
                    if got != p[0].request {
                        return Err(format!(
                            "{case}: params differ\n got: {}\nwant: {}",
                            serde_json::to_string_pretty(&got).unwrap(),
                            serde_json::to_string_pretty(&p[0].request).unwrap()
                        ));
                    }
                }
                let Some(go) = &c.go else { return Ok(()) };
                let o = query_options_of(c.go_options.as_ref(), true);
                let r = build()
                    .build(&v.table, o.query)
                    .map_err(|e| e.to_string())?;
                let mut run = Scripted::new(&c.responses);
                let res = r
                    .run(None, &o.run, &mut run)
                    .await
                    .map_err(|e| e.to_string())?;
                check_requests(&case, &run.requests, &go.requests)?;
                let mut data = Attrs::new();
                for (i, mem) in members.iter().enumerate() {
                    data.insert(
                        mem.alias.clone(),
                        Value::List(res.groups[i].iter().cloned().map(Value::Map).collect()),
                    );
                }
                let mut got = Attrs::new();
                got.insert("data".into(), Value::Map(data));
                got.insert(
                    "cursor".into(),
                    match res.next.as_ref().map(|n| encode_cursor(n).unwrap()) {
                        Some(Some(c)) => Value::String(c),
                        _ => Value::Null,
                    },
                );
                let got = Value::Map(got);
                let want = wattdb_vectors::attribute_value(go.result.as_ref().unwrap());
                if got != want {
                    return Err(format!(
                        "{case}: result differs\n got: {}\nwant: {}",
                        wire::to_json(&got),
                        wire::to_json(&want)
                    ));
                }
                Ok(())
            }
            .await;
            failures.check(r);
        }
    }
    failures.finish("collection", checked);
}
