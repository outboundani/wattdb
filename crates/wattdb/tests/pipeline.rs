//! Runs the recorded put inputs and stored items of the "pipeline" and
//! "callbacks" vectors through derived entities, their Rust callbacks and
//! their serde forms, comparing with what ElectroDB wrote and returned.

mod common;
mod fixtures;

use fixtures::doc::Doc;
use fixtures::task::Task;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use wattdb::Entity;
use wattdb::engine::wire;

/// Recorded inputs the typed API handles differently from ElectroDB, by
/// design, keyed by case name.
const DIVERGENCES: &[(&str, &str)] = &[
    (
        "null for a string",
        "Rust can't pass JS null for a string; None means missing",
    ),
    (
        "empty set",
        "D18: an empty set is written as missing, where JS throws",
    ),
    (
        "list item missing a required property",
        "D17: required map properties are plain values, so the input can't omit one",
    ),
];

/// Decodes a recorded input into the entity's Input, or explains why the
/// typed API can't express it: wrong JSON types, or attributes the Input
/// doesn't have.
fn input<E: Entity>(arg: &Json) -> Result<E::Input, String>
where
    E::Input: Serialize + DeserializeOwned,
{
    let input: E::Input = serde_json::from_value(arg.clone()).map_err(|e| e.to_string())?;
    let back = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    for k in arg.as_object().into_iter().flat_map(|o| o.keys()) {
        if back.get(k).is_none() && !arg[k].is_null() {
            return Err(format!("no attribute {k:?}"));
        }
    }
    Ok(input)
}

fn check_puts<E: Entity>(vector: &str) -> usize
where
    E::Input: Serialize + DeserializeOwned,
{
    let def = E::definition().unwrap();
    let v = wattdb_vectors::load_named(vector);
    let (mut checked, mut failures) = (0, Vec::new());
    for c in &v.cases {
        let Some(first) = c.chain.first() else {
            continue;
        };
        if (first.method != "put" && first.method != "create")
            || !first.args.first().is_some_and(Json::is_object)
        {
            continue;
        }
        if DIVERGENCES.iter().any(|(n, _)| *n == c.name) {
            continue;
        }
        let rejected = c.params_error.as_ref().or(c.error.as_ref());
        let want = c.requests().first().map(|r| &r.request["Item"]);
        if rejected.is_none() && want.is_none() {
            continue;
        }
        let input = match input::<E>(&first.args[0]) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("{vector}/{}: skipped, not expressible: {e}", c.name);
                continue;
            }
        };
        let got = E::encode_input(&input).and_then(|attrs| def.put_item(&attrs));
        match (rejected, got, want) {
            (Some(e), Ok(item), _) => failures.push(format!(
                "{}: ElectroDB rejected this ({}) but wattdb wrote {}",
                c.name,
                e.message,
                wire::attrs_to_json(&item)
            )),
            (Some(_), Err(_), _) => {}
            (None, Err(e), _) => failures.push(format!("{}: put_item: {e}", c.name)),
            (None, Ok(item), Some(want)) => {
                let got = wire::attrs_to_json(&item);
                if &got != want {
                    failures.push(format!(
                        "{}: items differ\n got: {got}\nwant: {want}",
                        c.name
                    ));
                }
            }
            (None, Ok(_), None) => unreachable!(),
        }
        checked += 1;
    }
    assert!(
        failures.is_empty(),
        "{vector}: {} of {checked} puts failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(checked > 0, "{vector}: no put cases checked");
    eprintln!("{vector}: checked {checked} puts");
    checked
}

/// JSON equality with numbers compared by value, so `2.0` equals `2`.
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => x.as_f64() == y.as_f64(),
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn check_reads<E: Entity + Serialize>(vector: &str) -> usize {
    let def = E::definition().unwrap();
    let v = wattdb_vectors::load_named(vector);
    let (mut checked, mut failures) = (0, Vec::new());
    for c in &v.cases {
        let Some(first) = c.chain.first() else {
            continue;
        };
        let Some(stored) = c.responses.first().and_then(|r| r.get("Item")) else {
            continue;
        };
        let Some(result) = c.go.as_ref().and_then(|g| g.result.as_ref()) else {
            continue;
        };
        let opts = c.go_options.clone().unwrap_or_default();
        if first.method != "get" || opts["data"] == "includeKeys" || !opts["attributes"].is_null() {
            continue;
        }
        let formatted = def
            .format_item(&wattdb_vectors::attribute_map(stored))
            .unwrap()
            .expect("an owned item");
        let got = serde_json::to_value(E::decode(&formatted).unwrap()).unwrap();
        let want = wire::to_plain_json(&wattdb_vectors::attribute_value(&result["M"]["data"]));
        if !same(&got, &want) {
            failures.push(format!(
                "{}: reads differ\n got: {got}\nwant: {want}",
                c.name
            ));
        }
        checked += 1;
    }
    assert!(
        failures.is_empty(),
        "{vector}: {} of {checked} reads failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(checked > 0, "{vector}: no read cases checked");
    checked
}

#[test]
fn doc_puts() {
    check_puts::<Doc>("pipeline");
}

#[test]
fn doc_reads() {
    check_reads::<Doc>("pipeline");
}

#[test]
fn task_puts() {
    check_puts::<Task>("callbacks");
}

#[test]
fn task_reads() {
    check_reads::<Task>("callbacks");
}
