# wattdb

A strongly typed DynamoDB library for Rust that reproduces [ElectroDB](https://electrodb.dev) 3.9.
Describe each entity as a struct and derive `Entity`. The items, keys, query results and cursors
wattdb reads and writes are the ones ElectroDB would, so Rust, Go and TypeScript services can share
a table while you port from one to another.

```rust
use wattdb::{Config, Entity, Enum};

#[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
#[wattdb(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    Done,
}

#[derive(Entity, Clone, Debug, Default)]
#[wattdb(entity = "task", version = "1", service = "taskapp", rename_all = "camelCase")]
#[wattdb(index(access_pattern = "primary",
    pk(field = "pk", composite(project_id)), sk(field = "sk", composite(task_id))))]
#[wattdb(index(access_pattern = "byAssignee", index = "gsi1",
    pk(field = "gsi1pk", composite(assignee)), sk(field = "gsi1sk", composite(status, task_id))))]
pub struct Task {
    #[wattdb(required)]
    pub project_id: String,
    #[wattdb(required)]
    pub task_id: String,
    #[wattdb(required)]
    pub title: String,
    #[wattdb(required, default = Status::Open)]
    pub status: Status,
    pub points: Option<i64>,
    pub assignee: Option<String>,
}

async fn run(ddb: aws_sdk_dynamodb::Client) -> wattdb::Result<()> {
    let cfg = Config::new(ddb).with_tracing();
    let tasks = TaskClient::new(&cfg, "app-table")?;

    tasks.create(TaskInput {
        project_id: "p1".into(),
        task_id: "t1".into(),
        title: "Write docs".into(),
        ..Default::default()
    }).exec().await?;

    tasks.update(TaskKey { project_id: "p1".into(), task_id: "t1".into() })
        .set(Task::status(), Status::Done)
        .add(Task::points(), 3)
        .where_(Task::status().eq(Status::Open))
        .exec()
        .await?;

    let page = tasks.by_assignee("ada").status().eq(Status::Open).limit(50).exec().await?;
    for task in page.items {
        println!("{} {}", task.task_id, task.title);
    }
    Ok(())
}
```

- **Typed everything:** inputs, keys, enums, attribute descriptors and sort-key steps.
  `Task::title().gt(5)` doesn't compile, and neither do sort-key steps out of order.
- **ElectroDB compatible:** the same stored bytes, identifiers (`__edb_e__`/`__edb_v__`), ownership
  filtering, validation and callback order, checked against vectors recorded from ElectroDB.
- **Built on Tokio and the AWS SDK for Rust:** builders end in `async fn exec()`, and pages and
  items stream as `futures::Stream`s.
- **The full feature set:** queries, scans, updates and upserts, conditions, projections,
  hydration, pagination and streams, batches, transactions, collections (clustered and
  sub-collections too), composite indexes, find/match, conversions, parse, hooks and migrations
  between versions.

## A tour

```rust,ignore
// Conditions and failures
match tasks.create(input).exec().await {
    Err(e) if e.is_condition_failed() => { /* it exists */ }
    other => { other?; }
}
let out = tasks.delete(key).return_rejected_with_old().exec().await?; // out.rejected, out.item

// Queries: partition composites as arguments, then sort-key steps
tasks.primary("p1").task_id().begins_with("t-2026").desc().max_pages(3).exec().await?;
tasks.primary("p1").where_(or([Task::points().gt(5), Task::assignee().not_exists()])).exec().await?;
let partials = tasks.primary("p1").into_query().attributes((Task::title(), Task::status())).exec().await?;
let mut all = std::pin::pin!(tasks.primary("p1").all()); // a Stream of every item

// Batches: chunked like ElectroDB, with optional concurrency and retries
let out = tasks.batch_get(keys).concurrent(4).exec().await?; // out.found, out.unprocessed
let left = tasks.batch_put(inputs).retry_unprocessed(Backoff::new(5, Duration::from_millis(50))).exec().await?;

// Transactions
wattdb::tx_write(&cfg, [
    tasks.update(key).set(Task::status(), Status::Done).tx_op(),
    projects.check(pkey).where_(Project::archived().not_exists()).tx_op(),
]).token(request_id).exec().await?;

// Collections: one query, one Vec per entity
#[derive(Collection, Debug, Default)]
#[wattdb(collection = "workspace")]
pub struct Workspace { pub projects: Vec<Project>, pub tasks: Vec<Task> }

let ws = WorkspaceClient::new(&cfg, "app-table")?.query(WorkspaceKey { project_id: "p1".into() }).exec().await?;

// Find/Match pick the best index the way ElectroDB does
tasks.matching(TaskPartial { project_id: Some("p1".into()), assignee: Some("ada".into()), ..Default::default() })
    .exec().await?;

// Hooks wrap every request
let cfg = Config::new(ddb).with_hook(|call: Call, next: Next| async move {
    println!("{} {:?}", call.operation(), call.entity);
    next.run(call).await
});
```

Every builder also has `.params()` to see the request, `.table(name)` and `.client(c)` per call,
and `.mutate(|input| ...)` for anything else.

[`examples/taskapp.rs`](examples/taskapp.rs) is a complete program; run it against DynamoDB Local
with `cargo run -p wattdb --example taskapp`.

## Porting from ElectroDB

Translate the model attribute by attribute, keeping the stored names with `rename_all` or
`field`, and keep the entity name, version, service, labels, casing and templates exactly as they
are. Times and UUIDs that JS also reads need string codecs:
`#[wattdb(codec = wattdb::codec::JsIso)]` and `#[wattdb(codec = wattdb::codec::UuidString)]`.

[docs/rust.md](../../docs/rust.md) covers the Rust API's decisions, and
[docs/compat.md](../../docs/compat.md) every place wattdb deliberately differs from ElectroDB.

## License

MIT
