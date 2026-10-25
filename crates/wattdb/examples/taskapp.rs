//! The taskapp example: projects and their tasks in one table.
//!
//! Run it against DynamoDB Local:
//!
//! ```sh
//! docker run -p 8000:8000 amazon/dynamodb-local:3.3.1
//! cargo run -p wattdb --example taskapp
//! ```
//!
//! The table needs pk/sk plus gsi1 (gsi1pk/gsi1sk) and gsi2 (gsi2pk/gsi2sk);
//! the program creates it when it's missing.

use std::time::Duration;

use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, GlobalSecondaryIndex, KeySchemaElement, KeyType, Projection,
    ProjectionType, ScalarAttributeType,
};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::prelude::*;
use wattdb::{Backoff, Collection, Config, Entity, Enum, Error, Service};

const TASKAPP: Service = Service::new("taskapp");

/// A project in an organization.
#[derive(Entity, Clone, Debug, Default, PartialEq)]
#[wattdb(entity = "project", version = "1", service = TASKAPP, rename_all = "camelCase")]
#[wattdb(index(
    access_pattern = "primary",
    pk(field = "pk", composite(org_id)),
    sk(field = "sk", composite(project_id))
))]
#[wattdb(index(
    access_pattern = "workspace",
    index = "gsi1",
    collection = "workspace",
    pk(field = "gsi1pk", composite(org_id, project_id)),
    sk(field = "gsi1sk", composite())
))]
pub struct Project {
    #[wattdb(required)]
    pub org_id: String,
    #[wattdb(required)]
    pub project_id: String,
    #[wattdb(required)]
    pub name: String,
    #[wattdb(required, read_only)]
    pub created_at: DateTime<Utc>,
}

#[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
#[wattdb(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    Doing,
    Done,
}

/// A task in a project.
#[derive(Entity, Clone, Debug, Default, PartialEq)]
#[wattdb(entity = "task", version = "1", service = TASKAPP, rename_all = "camelCase")]
#[wattdb(index(
    access_pattern = "primary",
    pk(field = "pk", composite(org_id, project_id)),
    sk(field = "sk", composite(task_id))
))]
#[wattdb(index(
    access_pattern = "workspace",
    index = "gsi1",
    collection = "workspace",
    pk(field = "gsi1pk", composite(org_id, project_id)),
    sk(field = "gsi1sk", composite(task_id))
))]
#[wattdb(index(
    access_pattern = "byAssignee",
    index = "gsi2",
    pk(field = "gsi2pk", composite(assignee)),
    sk(field = "gsi2sk", composite(status, task_id))
))]
pub struct Task {
    #[wattdb(required)]
    pub org_id: String,
    #[wattdb(required)]
    pub project_id: String,
    #[wattdb(required)]
    pub task_id: String,
    #[wattdb(required)]
    pub title: String,
    #[wattdb(required, default = Status::Open)]
    pub status: Status,
    pub points: Option<i64>,
    pub tags: Option<std::collections::BTreeSet<String>>,
    pub assignee: Option<String>,
}

/// A project and its tasks, read in one query.
#[derive(Collection, Debug, Default)]
#[wattdb(collection = "workspace")]
pub struct Workspace {
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Log wattdb's requests, not the SDK's internals.
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(Targets::new().with_target("wattdb", tracing::Level::DEBUG))
        .init();
    let endpoint =
        std::env::var("DYNAMODB_ENDPOINT").unwrap_or_else(|_| "http://localhost:8000".into());
    let conf = aws_sdk_dynamodb::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new("local", "local", None, None, "local"))
        .endpoint_url(endpoint)
        .build();
    let ddb = aws_sdk_dynamodb::Client::from_conf(conf);
    ensure_table(&ddb, "taskapp").await?;
    run(ddb, "taskapp").await
}

async fn run(ddb: aws_sdk_dynamodb::Client, table: &str) -> Result<(), Box<dyn std::error::Error>> {
    // One Config serves every client; tracing logs each request.
    let cfg = Config::new(ddb).with_tracing();
    let projects = ProjectClient::new(&cfg, table)?;
    let tasks = TaskClient::new(&cfg, table)?;

    // Create fails with a condition failure when the item exists.
    let web = ProjectInput {
        org_id: "acme".into(),
        project_id: "web".into(),
        name: "Website".into(),
        created_at: Utc::now(),
    };
    match projects.create(web).exec().await {
        Ok(_) => {}
        Err(e) if e.is_condition_failed() => {}
        Err(e) => return Err(e.into()),
    }

    // Batch writes chunk and can retry unprocessed items.
    let batch = ["Design", "Build", "Ship"]
        .iter()
        .enumerate()
        .map(|(i, title)| TaskInput {
            org_id: "acme".into(),
            project_id: "web".into(),
            task_id: format!("t{}", i + 1),
            title: title.to_string(),
            points: Some(i as i64 + 1),
            assignee: Some("ada".into()),
            ..Default::default()
        });
    let left = tasks
        .batch_put(batch)
        .retry_unprocessed(Backoff::new(5, Duration::from_millis(50)))
        .exec()
        .await?;
    assert!(left.is_empty(), "{} tasks left unprocessed", left.len());

    // Typed updates and conditions.
    let t1 = TaskKey {
        org_id: "acme".into(),
        project_id: "web".into(),
        task_id: "t1".into(),
    };
    match tasks
        .update(t1)
        .set(Task::status(), Status::Doing)
        .add(
            Task::tags(),
            std::collections::BTreeSet::from(["ux".to_string()]),
        )
        .where_(Task::status().eq(Status::Open))
        .exec()
        .await
    {
        Ok(()) => {}
        Err(e) if e.is_condition_failed() => {}
        Err(e) => return Err(e.into()),
    }

    // Queries take the partition key composites, then sort-key steps.
    let open = tasks
        .by_assignee("ada")
        .status()
        .eq(Status::Open)
        .exec()
        .await?;
    println!("ada has {} open tasks", open.items.len());

    // Transactions are all or nothing.
    let t2 = TaskKey {
        org_id: "acme".into(),
        project_id: "web".into(),
        task_id: "t2".into(),
    };
    let result = wattdb::tx_write(
        &cfg,
        [
            tasks.update(t2).set(Task::status(), Status::Done).tx_op(),
            projects
                .check(ProjectKey {
                    org_id: "acme".into(),
                    project_id: "web".into(),
                })
                .where_(Project::name().exists())
                .tx_op(),
        ],
    )
    .exec()
    .await;
    match result {
        Ok(()) => {}
        Err(Error::TxCanceled(canceled)) => println!("transaction canceled: {canceled}"),
        Err(e) => return Err(e.into()),
    }

    // A collection reads a project and its tasks in one query.
    let ws = WorkspaceClient::new(&cfg, table)?
        .query(WorkspaceKey {
            org_id: "acme".into(),
            project_id: "web".into(),
        })
        .exec()
        .await?
        .result;
    for p in &ws.projects {
        println!("project {} ({})", p.name, p.created_at.date_naive());
    }
    for t in &ws.tasks {
        println!("  {} {:<6} {}", t.task_id, t.status, t.title);
    }

    // Streams read every page.
    let mut all = std::pin::pin!(tasks.primary("acme", "web").all());
    let mut total = 0;
    while let Some(t) = all.next().await {
        total += t?.points.unwrap_or_default();
    }
    println!("total points: {total}");
    Ok(())
}

async fn ensure_table(
    ddb: &aws_sdk_dynamodb::Client,
    table: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if ddb.describe_table().table_name(table).send().await.is_ok() {
        return Ok(());
    }
    let def = |n: &str| {
        AttributeDefinition::builder()
            .attribute_name(n)
            .attribute_type(ScalarAttributeType::S)
            .build()
    };
    let key = |n: &str, kt| {
        KeySchemaElement::builder()
            .attribute_name(n)
            .key_type(kt)
            .build()
    };
    let gsi = |name: &str, pk: &str, sk: &str| {
        GlobalSecondaryIndex::builder()
            .index_name(name)
            .key_schema(key(pk, KeyType::Hash).unwrap())
            .key_schema(key(sk, KeyType::Range).unwrap())
            .projection(
                Projection::builder()
                    .projection_type(ProjectionType::All)
                    .build(),
            )
            .build()
    };
    let mut req = ddb
        .create_table()
        .table_name(table)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key("pk", KeyType::Hash)?)
        .key_schema(key("sk", KeyType::Range)?)
        .global_secondary_indexes(gsi("gsi1", "gsi1pk", "gsi1sk")?)
        .global_secondary_indexes(gsi("gsi2", "gsi2pk", "gsi2sk")?);
    for n in ["pk", "sk", "gsi1pk", "gsi1sk", "gsi2pk", "gsi2sk"] {
        req = req.attribute_definitions(def(n)?);
    }
    req.send().await?;
    Ok(())
}
