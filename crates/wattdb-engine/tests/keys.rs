//! Every key ElectroDB recorded: the key fields of put items, the Key of
//! get/delete/update requests, and query key values.

mod common;

use common::*;
use wattdb_engine::keys::{QueryType, primary_key, put_keys, query_keys};
use wattdb_engine::model::{IndexType, Model};
use wattdb_engine::{Attrs, wire};
use wattdb_vectors::Step;

fn item_attrs(m: &Model, item: &Attrs) -> Attrs {
    m.attributes
        .iter()
        .filter_map(|a| item.get(&a.field).map(|v| (a.name.clone(), v.clone())))
        .collect()
}

fn primary_attrs(m: &Model, attrs: &Attrs) -> Attrs {
    m.table_index()
        .composites()
        .into_iter()
        .filter_map(|n| attrs.get(&n).map(|v| (n, v.clone())))
        .collect()
}

fn key_fields(m: &Model, item: &Attrs) -> Attrs {
    let mut out = Attrs::new();
    for ix in &m.indexes {
        if ix.ty == IndexType::Composite {
            continue;
        }
        let mut fields = vec![&ix.pk.field];
        if ix.has_sk {
            fields.push(&ix.sk.field);
        }
        for f in fields {
            if let Some(v) = item.get(f) {
                out.insert(f.clone(), v.clone());
            }
        }
    }
    out
}

/// Merges the access-pattern composites with the sort-key clause, as
/// ElectroDB's _consolidateQueryFacets does.
fn query_facets(chain: &[Step], base: &Attrs) -> (QueryType, Vec<Attrs>) {
    let merge = |arg| {
        let mut out = base.clone();
        out.extend(wattdb_vectors::attrs(arg));
        out
    };
    for step in &chain[1..] {
        let typ = match step.method.as_str() {
            "between" => {
                return (
                    QueryType::Between,
                    vec![merge(&step.args[0]), merge(&step.args[1])],
                );
            }
            "begins" => QueryType::Begins,
            "gt" => QueryType::Gt,
            "gte" => QueryType::Gte,
            "lt" => QueryType::Lt,
            "lte" => QueryType::Lte,
            _ => continue,
        };
        return (typ, vec![merge(&step.args[0])]);
    }
    (QueryType::Is, vec![base.clone()])
}

fn show(m: &Attrs) -> String {
    serde_json::to_string(&wire::attrs_to_json(m)).unwrap()
}

#[test]
fn keys_match_vectors() {
    let mut failures = Failures::default();
    let mut checked = 0;
    for (v, models) in vectors() {
        for c in &v.cases {
            let Some(m) = model(&models, &c.entity) else {
                continue;
            };
            if c.error.is_some() || c.chain.is_empty() {
                continue;
            }
            let Some(req) = c.requests().first() else {
                continue;
            };
            let name = format!("{}/{}", v.name, c.name);
            let first = &c.chain[0];
            let (method, pattern) = first.method.split_once('.').unwrap_or((&first.method, ""));
            if first.args.is_empty() || !is_object(&first.args[0]) {
                continue; // scans and batch operations aren't key-level checks
            }
            let args = wattdb_vectors::attrs(&first.args[0]);
            let body = &req.request;
            let result: Result<(Attrs, Attrs), String> = match method {
                "put" | "create" if req.target == "PutItem" => {
                    let item = wattdb_vectors::attribute_map(&body["Item"]);
                    let set = item_attrs(m, &item);
                    put_keys(m, &set, &primary_attrs(m, &set))
                        .map(|got| (got, key_fields(m, &item)))
                        .map_err(|e| format!("put_keys: {e}"))
                }
                "get" | "delete" | "remove" | "update" | "patch" | "upsert" => {
                    primary_key(m, &args)
                        .map(|got| (got, wattdb_vectors::attribute_map(&body["Key"])))
                        .map_err(|e| format!("primary_key: {e}"))
                }
                "query" => {
                    let ix = m
                        .access_pattern(pattern)
                        .unwrap_or_else(|| panic!("unknown access pattern {pattern:?}"));
                    if ix.ty == IndexType::Composite {
                        continue; // no key fields; the query vectors cover these
                    }
                    let (typ, sks) = query_facets(&c.chain, &args);
                    let sk_refs: Vec<&Attrs> = sks.iter().collect();
                    match query_keys(m, ix, typ, &args, &sk_refs) {
                        Err(e) => Err(format!("query_keys: {e}")),
                        Ok(keys) => {
                            let values =
                                wattdb_vectors::attribute_map(&body["ExpressionAttributeValues"]);
                            let mut got = Attrs::new();
                            let mut want = Attrs::new();
                            got.insert(":pk".into(), keys.pk.clone().unwrap_or_default());
                            want.insert(":pk".into(), values[":pk"].clone());
                            for (i, sk) in keys.sk.iter().enumerate() {
                                let name = format!(":sk{}", i + 1);
                                if let Some(v) = values.get(&name) {
                                    got.insert(name.clone(), sk.clone());
                                    want.insert(name, v.clone());
                                }
                            }
                            Ok((got, want))
                        }
                    }
                }
                _ => continue,
            };
            checked += 1;
            failures.check(match result {
                Err(e) => Err(format!("{name}: {e}")),
                Ok((got, want)) if got != want => Err(format!(
                    "{name}: keys differ\n got: {}\nwant: {}",
                    show(&got),
                    show(&want)
                )),
                Ok(_) => Ok(()),
            });
        }
    }
    failures.finish("key", checked);
}
