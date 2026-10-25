//! Migrating between entity versions against DynamoDB Local.
#![cfg(feature = "integration")]

use wattdb::Config;
use wattdb_integration::local;
use wattdb_integration::task::{self, TaskClient, TaskInput};
use wattdb_integration::taskv2;

#[tokio::test]
async fn migrate() -> Result<(), Box<dyn std::error::Error>> {
    let ddb = local::client().await;
    let table = local::new_table(&ddb, &[]).await;
    let cfg = Config::new(ddb);
    let v1 = TaskClient::new(&cfg, &table)?;
    let v2 = taskv2::TaskClient::new(&cfg, &table)?;
    let ins: Vec<_> = (0..40)
        .map(|i| TaskInput {
            project_id: "p1".into(),
            task_id: format!("t{i:02}"),
            status: task::Status::Open,
            priority: Some(i),
            created_at: Some("2026-10-12".into()),
            assignee: (i % 2 == 0).then(|| "ada".into()),
            ..Default::default()
        })
        .collect();
    let left = v1.batch_put(ins).exec().await?;
    assert!(left.is_empty(), "seed: {} left", left.len());

    let n = wattdb::migrate(&v1, &v2, |old| {
        Ok(taskv2::TaskInput {
            project_id: old.project_id,
            task_id: old.task_id,
            title: old.title,
            status: match old.status {
                task::Status::Open => taskv2::Status::Open,
                task::Status::Done => taskv2::Status::Done,
            },
            priority: old
                .priority
                .filter(|p| *p >= 20)
                .map(|_| taskv2::Priority::High),
            assignees: old.assignee.map(|a| vec![a]),
        })
    })
    .await?;
    assert_eq!(n, 40);

    let got = v2
        .get(taskv2::TaskKey {
            project_id: "p1".into(),
            task_id: "t30".into(),
        })
        .exec()
        .await?
        .unwrap();
    assert_eq!(got.priority, taskv2::Priority::High);
    assert_eq!(got.assignees, Some(vec!["ada".to_string()]));
    // Each version only sees its own items.
    assert_eq!(v1.scan().all_pages().exec().await?.items.len(), 40);
    assert_eq!(v2.scan().all_pages().exec().await?.items.len(), 40);
    Ok(())
}
