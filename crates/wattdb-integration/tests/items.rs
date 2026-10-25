//! Writes, reads, conditions, updates and upserts against DynamoDB Local.
#![cfg(feature = "integration")]

use std::collections::HashMap;

use aws_sdk_dynamodb::types::AttributeValue;
use serde_json::Value as Json;
use wattdb::{Config, Error, ReturnMode, UpdatedMode, and};
use wattdb_integration::item::{Item, ItemClient, ItemInput, ItemKey, Meta, Status};
use wattdb_integration::local::{self, Gsi};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn table(c: &aws_sdk_dynamodb::Client) -> String {
    local::new_table(
        c,
        &[
            Gsi::new("gsi1", "gsi1pk", "gsi1sk"),
            Gsi::new("gsi2", "gsi2pk", "gsi2sk"),
        ],
    )
    .await
}

async fn setup() -> (aws_sdk_dynamodb::Client, String, ItemClient) {
    let ddb = local::client();
    let t = table(&ddb).await;
    let items = ItemClient::new(&Config::new(ddb.clone()), &t).unwrap();
    (ddb, t, items)
}

async fn raw(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
    org: &str,
    id: &str,
) -> HashMap<String, AttributeValue> {
    let key = [
        (
            "pk".to_string(),
            AttributeValue::S(format!("$taskapp#orgid_{}", org.to_lowercase())),
        ),
        (
            "sk".to_string(),
            AttributeValue::S(format!("$item_1#itemid_{}", id.to_lowercase())),
        ),
    ];
    let out = ddb
        .get_item()
        .table_name(table)
        .set_key(Some(key.into()))
        .consistent_read(true)
        .send()
        .await
        .unwrap();
    out.item.unwrap_or_default()
}

fn base() -> ItemInput {
    ItemInput {
        org_id: "O1".into(),
        item_id: "I1".into(),
        title: "T".into(),
        created_at: Some("2026-01-01".into()),
        ..Default::default()
    }
}

fn key() -> ItemKey {
    ItemKey {
        org_id: "O1".into(),
        item_id: "I1".into(),
    }
}

fn s(v: &AttributeValue) -> &str {
    v.as_s().unwrap()
}

/// Writes each recorded put/create input through the derived client and
/// compares the stored item with ElectroDB's.
#[tokio::test]
async fn puts_match_electrodb() -> Result {
    let v = wattdb_vectors::load_named("mutations");
    let mut checked = 0;
    for c in &v.cases {
        let first = &c.chain[0];
        let Some(params) = &c.params else { continue };
        if (first.method != "put" && first.method != "create") || c.chain.len() != 1 {
            continue;
        }
        let arg = &first.args[0];
        let input: ItemInput =
            serde_json::from_value(arg.clone()).map_err(|e| format!("{}: {e}", c.name))?;
        let back = serde_json::to_value(&input)?;
        for k in arg.as_object().unwrap().keys() {
            assert!(
                back.get(k).is_some(),
                "{}: the input has no attribute {k:?}",
                c.name
            );
        }
        let (ddb, t, items) = setup().await;
        let (org, id) = (input.org_id.clone(), input.item_id.clone());
        if first.method == "create" {
            items.create(input).exec().await?;
        } else {
            items.put(input).exec().await?;
        }
        let want: HashMap<String, AttributeValue> =
            wattdb::to_item(&wattdb_vectors::attribute_map(&params[0].request["Item"]))?;
        assert_eq!(
            raw(&ddb, &t, &org, &id).await,
            want,
            "{}: stored item differs",
            c.name
        );
        checked += 1;
    }
    assert!(checked > 0, "no cases checked");
    Ok(())
}

#[tokio::test]
async fn get_and_delete() -> Result {
    let (_, _, items) = setup().await;
    assert_eq!(items.get(key()).exec().await?, None);
    let put = items
        .put(ItemInput {
            tags: Some(vec!["b".into(), "a".into(), "b".into()]),
            meta: Some(Meta {
                a: Some("x".into()),
                b: None,
            }),
            ..base()
        })
        .exec()
        .await?;
    assert_eq!(put.status, Status::Open);
    let got = items
        .get(key())
        .consistent()
        .exec()
        .await?
        .expect("the item");
    assert_eq!((got.title.as_str(), &got.status), ("T", &Status::Open));
    let mut tags = got.tags.unwrap();
    tags.sort();
    assert_eq!(tags, ["a", "b"]);
    items.delete(key()).exec().await?;
    assert_eq!(items.get(key()).exec().await?, None);
    Ok(())
}

#[tokio::test]
async fn condition_failures() -> Result {
    let (_, _, items) = setup().await;
    let failed = |r: wattdb::Result<()>| matches!(r, Err(e) if e.is_condition_failed());

    assert!(
        failed(items.remove(key()).exec().await),
        "remove on a missing item"
    );
    assert!(
        failed(items.patch(key()).set(Item::title(), "x").exec().await),
        "patch on a missing item"
    );
    items.create(base()).exec().await?;
    assert!(
        failed(items.create(base()).exec().await.map(|_| ())),
        "second create"
    );

    let o = items
        .create(base())
        .return_rejected_with_old()
        .exec()
        .await?;
    assert!(
        o.rejected && o.item.as_ref().is_some_and(|i| i.title == "T"),
        "with old: {o:?}"
    );
    let o = items.create(base()).return_rejected().exec().await?;
    assert!(o.rejected && o.item.is_none(), "{o:?}");

    let r = items
        .update(key())
        .set(Item::title(), "y")
        .where_(Item::status().eq(Status::Closed))
        .exec()
        .await;
    assert!(failed(r), "update with a false condition");
    items
        .update(key())
        .set(Item::title(), "y")
        .where_(and([
            Item::status().eq(Status::Open),
            Item::title().begins_with("T"),
        ]))
        .exec()
        .await?;
    Ok(())
}

#[tokio::test]
async fn updates() -> Result {
    let (ddb, t, items) = setup().await;
    items
        .put(ItemInput {
            count: Some(5),
            blob: Some(serde_json::json!({ "x": 1 })),
            history: Some(vec!["h0".into()]),
            meta: Some(Meta {
                a: Some("a".into()),
                b: None,
            }),
            ..base()
        })
        .exec()
        .await?;

    items
        .update(key())
        .set(Item::title(), "New")
        .set(Item::meta().a(), "b")
        .add(Item::count(), 2)
        .add(Item::tags(), vec!["t1".into(), "t2".into()])
        .append(Item::history(), vec!["h1".into()])
        .if_not_exists(Item::meta().b(), 9)
        .remove(Item::blob())
        .exec()
        .await?;
    // DynamoDB rejects overlapping paths in one update, so the list item is
    // set separately.
    items
        .update(key())
        .set(Item::history().at(0), "first")
        .exec()
        .await?;
    let got = items.get(key()).consistent().exec().await?.unwrap();
    let meta = got.meta.clone().unwrap();
    assert_eq!(got.title, "New");
    assert_eq!(got.count, Some(7));
    assert_eq!((meta.a.as_deref(), meta.b), (Some("b"), Some(9)));
    assert_eq!(
        got.history.as_deref(),
        Some(&["first".to_string(), "h1".to_string()][..])
    );
    assert_eq!(got.blob, None);
    assert_eq!(got.tags.as_ref().map(Vec::len), Some(2));

    items
        .update(key())
        .subtract(Item::count(), 10)
        .delete(Item::tags(), vec!["t1".into()])
        .exec()
        .await?;
    let got = items.get(key()).consistent().exec().await?.unwrap();
    assert_eq!(
        (got.count, got.tags),
        (Some(-3), Some(vec!["t2".to_string()]))
    );

    // Sparse index: setting the owner writes gsi2 keys; removing it removes
    // gsi2pk but, as in ElectroDB, not the composite-less gsi2sk.
    items
        .update(key())
        .set(Item::owner_id(), "U1")
        .exec()
        .await?;
    let r = raw(&ddb, &t, "O1", "I1").await;
    assert!(
        r.contains_key("gsi2pk") && r.contains_key("gsi2sk"),
        "sparse keys missing after set: {r:?}"
    );
    items.update(key()).remove(Item::owner_id()).exec().await?;
    let r = raw(&ddb, &t, "O1", "I1").await;
    assert!(
        !r.contains_key("gsi2pk") && r.contains_key("gsi2sk"),
        "after removing the owner: {r:?}"
    );

    // A gsi composite without its sibling fails; composite() supplies it.
    let err = items
        .update(key())
        .set(Item::status(), Status::Closed)
        .exec()
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::IncompleteComposites(_)),
        "update of one gsi composite: {err:?}"
    );
    items
        .update(key())
        .set(Item::status(), Status::Closed)
        .composite(Item::created_at(), "2026-01-01")
        .exec()
        .await?;
    let r = raw(&ddb, &t, "O1", "I1").await;
    assert_eq!(
        s(&r["gsi1sk"]),
        "$item_1#status_closed#createdat_2026-01-01"
    );

    // Validation and readOnly errors come back from exec.
    let err = items
        .update(key())
        .set(Item::created_by(), "x")
        .exec()
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Validation(_)),
        "update of a readOnly attribute: {err:?}"
    );
    let err = items
        .update(key())
        .remove(Item::title())
        .exec()
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Validation(_)),
        "removing a required attribute: {err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn returning() -> Result {
    let (_, _, items) = setup().await;
    items.put(base()).exec().await?;
    let after = items
        .update(key())
        .set(Item::title(), "A")
        .returning(ReturnMode::AllNew)
        .exec()
        .await?;
    assert_eq!(after.map(|i| i.title).as_deref(), Some("A"));
    let old = items
        .update(key())
        .set(Item::title(), "B")
        .returning_updated(UpdatedMode::UpdatedOld)
        .exec()
        .await?
        .unwrap();
    assert_eq!((old.title.as_deref(), &old.status), (Some("A"), &None));
    let gone = items
        .delete(key())
        .returning(ReturnMode::AllOld)
        .exec()
        .await?;
    assert_eq!(gone.map(|i| i.title).as_deref(), Some("B"));
    assert_eq!(
        items
            .delete(key())
            .returning(ReturnMode::AllOld)
            .exec()
            .await?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn upsert() -> Result {
    let (_, _, items) = setup().await;
    let first = ItemInput {
        created_by: Some("u1".into()),
        ..base()
    };
    items
        .upsert(first.clone())
        .add(Item::count(), 1)
        .exec()
        .await?;
    let second = ItemInput {
        created_by: Some("u2".into()),
        title: "T2".into(),
        ..first
    };
    items.upsert(second).add(Item::count(), 1).exec().await?;
    let got = items.get(key()).consistent().exec().await?.unwrap();
    // createdBy is readOnly: upsert only sets it if missing.
    assert_eq!(
        (got.title.as_str(), got.created_by.as_deref(), got.count),
        ("T2", Some("u1"), Some(2))
    );
    Ok(())
}

/// Stores an item exactly as ElectroDB wrote it and reads it through the
/// derived client.
#[tokio::test]
async fn reads_electrodb_item() -> Result {
    let v = wattdb_vectors::load_named("mutations");
    let c = v
        .cases
        .iter()
        .find(|c| c.name == "create")
        .expect("the create case");
    let stored: &Json = &c.params.as_ref().unwrap()[0].request["Item"];
    let (ddb, t, items) = setup().await;
    let item = wattdb::to_item(&wattdb_vectors::attribute_map(stored))?;
    ddb.put_item()
        .table_name(&t)
        .set_item(Some(item))
        .send()
        .await?;
    let got = items.get(key()).exec().await?.unwrap();
    assert_eq!(
        (
            got.org_id.as_str(),
            got.title.as_str(),
            &got.status,
            got.created_at.as_deref()
        ),
        ("O1", "T", &Status::Open, Some("2026-01-01"))
    );
    Ok(())
}
