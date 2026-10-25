//! Queries, pagination, index projections and scans against DynamoDB Local.
#![cfg(feature = "integration")]

use aws_sdk_dynamodb::types::{AttributeValue, ProjectionType};
use base64::Engine;
use futures::StreamExt;
use wattdb::{CompareMode, Config, Cursor, Error};
use wattdb_integration::local::{self, Gsi};
use wattdb_integration::task::{Status, Task, TaskClient, TaskInput};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A task table seeded with tasks T1..T5 in project P1 and one in P2, plus
/// a foreign item shaped like a task.
async fn seeded() -> TaskClient {
    let ddb = local::client();
    let table = local::new_table(
        &ddb,
        &[
            Gsi::new("gsi1", "gsi1pk", "gsi1sk"),
            Gsi::new("gsi2", "gsi2pk", "gsi2sk").projection(ProjectionType::KeysOnly, &[]),
            Gsi::new("gsi3", "gsi3pk", "gsi3sk"),
            Gsi::new("gsi4", "gsi4pk", "gsi4sk").projection(ProjectionType::Include, &["title"]),
        ],
    )
    .await;
    let tasks = TaskClient::new(&Config::new(ddb.clone()), &table).unwrap();
    let t = |project: &str,
             id: &str,
             title: &str,
             status: Status,
             priority: i64,
             day: u8,
             assignee: Option<&str>| {
        TaskInput {
            project_id: project.into(),
            task_id: id.into(),
            title: Some(title.into()),
            status,
            priority: Some(priority),
            created_at: Some(format!("2026-01-0{day}")),
            assignee: assignee.map(Into::into),
            secret: None,
        }
    };
    let mut first = t("P1", "T1", "Alpha", Status::Open, 2, 1, Some("U1"));
    first.secret = Some("s".into());
    for input in [
        first,
        t("P1", "T2", "Beta", Status::Open, 5, 2, Some("U1")),
        t("P1", "T3", "Gamma", Status::Done, 1, 3, None),
        t("P1", "T4", "Delta", Status::Open, 9, 4, None),
        t("P1", "T5", "Epsilon", Status::Done, 3, 5, None),
        t("P2", "T6", "Other", Status::Open, 1, 6, None),
    ] {
        tasks.put(input).exec().await.unwrap();
    }
    // A foreign entity's item in the same partition.
    let s = |v: &str| AttributeValue::S(v.into());
    ddb.put_item()
        .table_name(&table)
        .item("pk", s("$taskapp#projectid_p1"))
        .item("sk", s("$task_1#taskid_t9"))
        .item("projectId", s("P1"))
        .item("taskId", s("T9"))
        .item("__edb_e__", s("other"))
        .item("__edb_v__", s("1"))
        .send()
        .await
        .unwrap();
    tasks
}

fn ids(items: &[Task]) -> Vec<&str> {
    items.iter().map(|t| t.task_id.as_str()).collect()
}

#[tokio::test]
async fn queries() -> Result {
    let c = seeded().await;

    let items = c.primary("P1").exec().await?.items;
    assert_eq!(
        ids(&items),
        ["T1", "T2", "T3", "T4", "T5"],
        "primary (foreign item dropped)"
    );
    assert_eq!(items[0].secret, None, "hidden attribute returned");
    let items = c.primary("P1").task_id().eq("T3").exec().await?.items;
    assert_eq!(ids(&items), ["T3"], "full key");
    let items = c
        .by_status("P1")
        .status()
        .eq(Status::Open)
        .into_query()
        .exec()
        .await?
        .items;
    assert_eq!(ids(&items), ["T1", "T2", "T4"], "partial sk");
    // The default "keys" comparison compares built key strings, as
    // ElectroDB does: "...#priority_00002#createdat_..." sorts after
    // "...#priority_00002", so gt(2) includes T1 and between(2, 5)
    // excludes T2; lte(5) doesn't bound status, so "done" items (which sort
    // before "open") match too.
    let open = || c.by_status("P1").status().eq(Status::Open);
    assert_eq!(
        ids(&open().priority().gt(2).exec().await?.items),
        ["T1", "T2", "T4"],
        "gt"
    );
    assert_eq!(
        ids(&open().priority().between(2, 5).exec().await?.items),
        ["T1"],
        "between"
    );
    assert_eq!(
        ids(&open().priority().lte(5).desc().exec().await?.items),
        ["T1", "T5", "T3"],
        "lte desc"
    );
    // With compare attributes, the composites are also filtered.
    let attrs = |q: wattdb::Query<Task>| q.compare(CompareMode::Attributes);
    assert_eq!(
        ids(&attrs(open().priority().gt(2)).exec().await?.items),
        ["T2", "T4"],
        "gt, attributes"
    );
    assert_eq!(
        ids(&attrs(open().priority().lte(5)).exec().await?.items),
        ["T1"],
        "lte, attributes"
    );
    assert_eq!(
        ids(&attrs(open().priority().gte(5)).exec().await?.items),
        ["T2", "T4"],
        "gte, attributes"
    );
    let items = c
        .by_status("P1")
        .status()
        .begins_with_prefix("d")
        .exec()
        .await?
        .items;
    assert_eq!(ids(&items), ["T3", "T5"], "begins on the first composite");
    let items = c
        .by_status("P1")
        .where_(Task::title().begins_with("G"))
        .exec()
        .await?
        .items;
    assert_eq!(ids(&items), ["T3"], "filter");
    let items = c.by_status_cluster(Status::Done).exec().await?.items;
    assert_eq!(ids(&items), ["T3", "T5"], "clustered");

    let parts = c
        .by_status("P1")
        .status()
        .eq(Status::Done)
        .into_query()
        .attributes(Task::title())
        .exec()
        .await?
        .items;
    assert!(
        parts.len() == 2 && parts[0].title.is_some() && parts[0].task_id.is_none(),
        "attributes: {parts:?}"
    );
    let keyed = c
        .primary("P1")
        .task_id()
        .eq("T1")
        .include_keys()
        .exec()
        .await?
        .items;
    assert!(
        keyed.len() == 1
            && keyed[0].keys.contains_key("pk")
            && keyed[0].keys.contains_key("__edb_e__"),
        "include_keys: {keyed:?}"
    );
    let outs = c.primary("P1").raw().await?;
    assert_eq!(outs.len(), 1);
    assert_eq!(outs[0].items().len(), 6);
    Ok(())
}

#[tokio::test]
async fn pagination() -> Result {
    let c = seeded().await;

    // Limit 2 per request: one page by default, with a cursor.
    let page = c.primary("P1").limit(2).exec().await?;
    assert_eq!(ids(&page.items), ["T1", "T2"], "first page");
    let cur = page.cursor.expect("a cursor after the first page");
    let page = c.primary("P1").limit(2).cursor(cur).exec().await?;
    assert_eq!(ids(&page.items), ["T3", "T4"], "second page");
    let page = c.primary("P1").limit(2).all_pages().exec().await?;
    assert_eq!(
        ids(&page.items),
        ["T1", "T2", "T3", "T4", "T5"],
        "all pages"
    );
    let page = c.primary("P1").limit(2).count(3).exec().await?;
    assert_eq!(ids(&page.items), ["T1", "T2", "T3"], "count");
    let page = c
        .primary("P1")
        .cursor(page.cursor)
        .all_pages()
        .exec()
        .await?;
    assert_eq!(ids(&page.items), ["T4", "T5"], "after count");

    let pages: Vec<Vec<String>> = c
        .primary("P1")
        .limit(2)
        .pages()
        .map(|p| p.unwrap().items.into_iter().map(|t| t.task_id).collect())
        .collect()
        .await;
    assert!(
        pages.len() >= 3 && pages[0] == ["T1", "T2"],
        "pages = {pages:?}"
    );
    let all: Vec<Task> = c
        .by_status("P1")
        .limit(1)
        .all()
        .map(|t| t.unwrap())
        .collect()
        .await;
    assert_eq!(all.len(), 5, "all = {:?}", ids(&all));

    // A cursor in ElectroDB's own form.
    let js = r#"{"pk":"$taskapp#projectid_p1","sk":"$task_1#taskid_t2"}"#;
    let cur = Cursor::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(js));
    let page = c.primary("P1").cursor(cur).all_pages().exec().await?;
    assert_eq!(ids(&page.items), ["T3", "T4", "T5"], "ElectroDB cursor");
    let err = c
        .primary("P1")
        .cursor(Cursor::new("%%%"))
        .exec()
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidCursor(_)),
        "bad cursor: {err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn index_projections() -> Result {
    let c = seeded().await;

    // A keys-only index can't check identifiers; items come back empty
    // unless hydrated.
    let items = c.by_assignee("U1").into_query().exec().await?.items;
    assert!(
        items.len() == 2 && items[0].task_id.is_empty(),
        "keys-only query: {items:?}"
    );
    let items = c
        .by_assignee("U1")
        .into_query()
        .hydrate()
        .exec()
        .await?
        .items;
    assert_eq!(ids(&items), ["T1", "T2"], "hydrated");
    assert_eq!(items[0].title.as_deref(), Some("Alpha"));
    let items = c.by_title("P1").into_query().exec().await?.items;
    assert!(
        items.len() == 5 && items[0].title.is_some(),
        "include-projection query: {items:?}"
    );
    Ok(())
}

#[tokio::test]
async fn scans() -> Result {
    let c = seeded().await;

    let items = c.scan().all_pages().exec().await?.items;
    assert_eq!(items.len(), 6, "scan: {:?}", ids(&items));
    let items = c
        .scan()
        .where_(Task::status().eq(Status::Done))
        .all_pages()
        .exec()
        .await?
        .items;
    assert_eq!(items.len(), 2, "filtered scan: {:?}", ids(&items));
    let items = c.scan_by_status_cluster().all_pages().exec().await?.items;
    assert_eq!(items.len(), 6, "index scan: {:?}", ids(&items));
    let input = c.scan().limit(3).params()?;
    assert_eq!(input.limit, Some(3));
    assert_eq!(input.expression_attribute_names.unwrap()["#pk"], "pk");
    Ok(())
}
