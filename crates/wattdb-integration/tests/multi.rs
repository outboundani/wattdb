//! Batches, transactions, collections, find/match and a composite index
//! against DynamoDB Local.
#![cfg(feature = "integration")]

use std::time::Duration;

use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, GlobalSecondaryIndex, KeySchemaElement, KeyType, Projection,
    ProjectionType, ScalarAttributeType,
};
use futures::StreamExt;
use wattdb::{Backoff, Config, Error, TxGet, tx_write};
use wattdb_integration::local::{self, Gsi};
use wattdb_integration::member::{
    self, Member, MemberClient, MemberInput, MemberKey, MemberPartial,
};
use wattdb_integration::project::{self, Project, ProjectClient, ProjectInput, ProjectKey};
use wattdb_integration::taskapp::{
    BoardClient, BoardKey, OrgClient, OrgKey, OrgMembersClient, OrgMembersKey, TeamClient, TeamKey,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn setup() -> (Config, String, MemberClient, ProjectClient) {
    let ddb = local::client().await;
    let table = local::new_table(
        &ddb,
        &[
            Gsi::new("gsi1", "gsi1pk", "gsi1sk"),
            Gsi::new("gsi2", "gsi2pk", "gsi2sk"),
            Gsi::new("gsi3", "gsi3pk", "gsi3sk"),
        ],
    )
    .await;
    let cfg = Config::new(ddb);
    let m = MemberClient::new(&cfg, &table).unwrap();
    let p = ProjectClient::new(&cfg, &table).unwrap();
    (cfg, table, m, p)
}

fn some(s: &str) -> Option<String> {
    Some(s.into())
}

fn mkey(id: &str) -> MemberKey {
    MemberKey {
        org_id: "O1".into(),
        member_id: id.into(),
    }
}

fn member(org: &str, id: &str, name: &str) -> MemberInput {
    MemberInput {
        org_id: org.into(),
        member_id: id.into(),
        name: some(name),
        ..Default::default()
    }
}

#[tokio::test]
async fn batches() -> Result {
    let (_, _, members, _) = setup().await;
    let ins: Vec<_> = (0..60)
        .map(|i| MemberInput {
            org_id: "O1".into(),
            member_id: format!("M{i:02}"),
            visits: Some(i),
            ..Default::default()
        })
        .collect();
    let keys: Vec<_> = (0..60).map(|i| mkey(&format!("M{i:02}"))).collect();
    let left = members
        .batch_put(ins)
        .concurrent(3)
        .retry_unprocessed(Backoff::new(5, Duration::from_millis(10)))
        .exec()
        .await?;
    assert!(left.is_empty(), "unprocessed {left:?}");

    // 60 stored plus 50 missing keys: two chunks.
    let mut all = keys.clone();
    all.extend((0..50).map(|i| mkey(&format!("X{i:02}"))));
    let out = members.batch_get(all).concurrent(2).exec().await?;
    assert!(
        out.unprocessed.is_empty(),
        "unprocessed {:?}",
        out.unprocessed
    );
    assert_eq!(out.found.len(), 60);

    let out = members
        .batch_get([keys[5].clone(), mkey("nope"), keys[2].clone()])
        .preserve_order()
        .exec()
        .await?;
    let ids: Vec<_> = out
        .items
        .iter()
        .map(|m| m.as_ref().map(|m| m.member_id.as_str()))
        .collect();
    assert_eq!(ids, [Some("M05"), None, Some("M02")]);
    assert_eq!(out.items[0].as_ref().unwrap().visits, Some(5));

    let left = members.batch_delete(keys[..30].to_vec()).exec().await?;
    assert!(left.is_empty(), "unprocessed {left:?}");
    let out = members.batch_get(keys).exec().await?;
    assert_eq!(out.found.len(), 30, "after delete");
    Ok(())
}

#[tokio::test]
async fn transactions() -> Result {
    let (cfg, _, members, projects) = setup().await;
    tx_write(
        &cfg,
        [
            members.put(member("O1", "M1", "Ada")).tx_op(),
            projects
                .create(ProjectInput {
                    org_id: "O1".into(),
                    project_id: "P1".into(),
                    name: some("Apollo"),
                    ..Default::default()
                })
                .tx_op(),
        ],
    )
    // A token unique to this run: DynamoDB Local may outlive it.
    .token(format!(
        "tx-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ))
    .exec()
    .await?;

    let mut tx = TxGet::new(&cfg);
    let m = tx.add(members.get(mkey("M1")).tx_get());
    let p = tx.add(
        projects
            .get(ProjectKey {
                org_id: "O1".into(),
                project_id: "P1".into(),
            })
            .tx_get(),
    );
    let missing = tx.add(
        projects
            .get(ProjectKey {
                org_id: "O1".into(),
                project_id: "P9".into(),
            })
            .tx_get(),
    );
    let out = tx.exec().await?;
    assert_eq!(out.get(m)?.and_then(|m| m.name).as_deref(), Some("Ada"));
    assert_eq!(out.get(p)?.and_then(|p| p.name).as_deref(), Some("Apollo"));
    assert_eq!(out.get(missing)?, None);

    // The check fails, so nothing is written; the create's old item comes
    // back with the reasons.
    let err = tx_write(
        &cfg,
        [
            members.update(mkey("M1")).set(Member::name(), "Bo").tx_op(),
            members
                .check(mkey("M1"))
                .where_(Member::visits().exists())
                .tx_op(),
            projects
                .create(ProjectInput {
                    org_id: "O1".into(),
                    project_id: "P1".into(),
                    ..Default::default()
                })
                .tx_op_return_old(),
        ],
    )
    .exec()
    .await
    .unwrap_err();
    let Error::TxCanceled(tce) = err else {
        panic!("err = {err:?}, want TxCanceled")
    };
    let codes: Vec<_> = tce.reasons.iter().map(|r| r.code.as_str()).collect();
    assert_eq!(
        codes,
        ["None", "ConditionalCheckFailed", "ConditionalCheckFailed"]
    );
    assert_eq!(
        tce.reasons[2]
            .item::<Project>()
            .and_then(|p| p.name.as_deref()),
        Some("Apollo")
    );
    let got = members.get(mkey("M1")).exec().await?.unwrap();
    assert_eq!(
        got.name.as_deref(),
        Some("Ada"),
        "after the canceled transaction"
    );
    Ok(())
}

#[tokio::test]
async fn collections() -> Result {
    let (cfg, table, members, projects) = setup().await;
    let away = MemberInput {
        status: Some(member::Status::Away),
        ..member("O1", "M2", "Bo")
    };
    for input in [member("O1", "M1", "Ada"), away, member("O2", "M3", "Cy")] {
        members.put(input).exec().await?;
    }
    let project = |id: &str, name: &str| ProjectInput {
        org_id: "O1".into(),
        project_id: id.into(),
        name: some(name),
        ..Default::default()
    };
    let zeus = ProjectInput {
        status: Some(project::Status::Away),
        ..project("P2", "Zeus")
    };
    for input in [project("P1", "Apollo"), zeus] {
        projects.put(input).exec().await?;
    }

    let team = TeamClient::new(&cfg, &table)?;
    let key = || TeamKey {
        org_id: "O1".into(),
    };
    let page = team.query(key()).exec().await?;
    assert!(page.cursor.is_none());
    assert_eq!(
        (page.result.members.len(), page.result.projects.len()),
        (2, 2),
        "team = {:?}",
        page.result
    );
    let r = team
        .query(key())
        .where_(Member::name().eq("Ada"))
        .exec()
        .await?
        .result;
    assert_eq!(
        (r.members.len(), r.projects.len()),
        (1, 0),
        "filtered team = {r:?}"
    );

    // Pages: one item per page.
    let pages: Vec<_> = team
        .query(key())
        .limit(1)
        .pages()
        .map(|p| p.unwrap())
        .collect()
        .await;
    let total: usize = pages
        .iter()
        .map(|p| p.result.members.len() + p.result.projects.len())
        .sum();
    assert!(
        total == 4 && pages.len() >= 4,
        "pages = {}, items = {total}",
        pages.len()
    );

    let board = BoardClient::new(&cfg, &table)?;
    let b = board
        .query(BoardKey {
            org_id: "O1".into(),
        })
        .status()
        .eq(member::Status::Away)
        .exec()
        .await?
        .result;
    assert_eq!(
        b.members
            .iter()
            .map(|m| m.member_id.as_str())
            .collect::<Vec<_>>(),
        ["M2"]
    );
    assert_eq!(
        b.projects
            .iter()
            .map(|p| p.project_id.as_str())
            .collect::<Vec<_>>(),
        ["P2"]
    );
    let b = board
        .query(BoardKey {
            org_id: "O1".into(),
        })
        .status()
        .eq(member::Status::Away)
        .member_id()
        .eq("M2")
        .exec()
        .await?
        .result;
    assert_eq!(
        (b.members.len(), b.projects.len()),
        (1, 0),
        "board M2 = {b:?}"
    );

    let om = OrgMembersClient::new(&cfg, &table)?
        .query(OrgMembersKey {
            org_id: "O1".into(),
        })
        .exec()
        .await?
        .result;
    assert_eq!(om.members.len(), 2, "orgMembers = {om:?}");
    let all = OrgClient::new(&cfg, &table)?
        .query(OrgKey {
            org_id: "O1".into(),
        })
        .exec()
        .await?
        .result;
    assert_eq!(
        (all.members.len(), all.projects.len()),
        (2, 2),
        "org = {all:?}"
    );
    Ok(())
}

#[tokio::test]
async fn find_and_match() -> Result {
    let (_, _, members, _) = setup().await;
    for input in [
        MemberInput {
            visits: Some(3),
            ..member("O1", "M1", "Ada")
        },
        MemberInput {
            visits: Some(3),
            ..member("O1", "M2", "Bo")
        },
        member("O2", "M3", "Ada"),
    ] {
        members.put(input).exec().await?;
    }
    let got = members
        .find(MemberPartial {
            org_id: some("O1"),
            member_id: some("M2"),
            ..Default::default()
        })
        .exec()
        .await?;
    assert_eq!(
        got.items
            .iter()
            .map(|m| m.member_id.as_str())
            .collect::<Vec<_>>(),
        ["M2"],
        "find by key"
    );
    let got = members
        .matching(MemberPartial {
            org_id: some("O1"),
            name: some("Ada"),
            ..Default::default()
        })
        .exec()
        .await?;
    assert_eq!(
        got.items
            .iter()
            .map(|m| m.member_id.as_str())
            .collect::<Vec<_>>(),
        ["M1"],
        "match"
    );
    let got = members
        .matching(MemberPartial {
            name: some("Ada"),
            ..Default::default()
        })
        .all_pages()
        .exec()
        .await?;
    assert_eq!(got.items.len(), 2, "match by scan");
    Ok(())
}

#[tokio::test]
async fn composite_index() -> Result {
    let ddb = local::client().await;
    let table = local::table_name();
    let def = |n: &str| {
        AttributeDefinition::builder()
            .attribute_name(n)
            .attribute_type(ScalarAttributeType::S)
            .build()
            .unwrap()
    };
    let key = |n: &str, kt| {
        KeySchemaElement::builder()
            .attribute_name(n)
            .key_type(kt)
            .build()
            .unwrap()
    };
    let created = ddb
        .create_table()
        .table_name(&table)
        .billing_mode(BillingMode::PayPerRequest)
        .set_attribute_definitions(Some(
            ["pk", "sk", "orgId", "owner", "due", "projectId"]
                .map(def)
                .to_vec(),
        ))
        .key_schema(key("pk", KeyType::Hash))
        .key_schema(key("sk", KeyType::Range))
        .global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name("gsi4")
                .key_schema(key("orgId", KeyType::Hash))
                .key_schema(key("owner", KeyType::Hash))
                .key_schema(key("due", KeyType::Range))
                .key_schema(key("projectId", KeyType::Range))
                .projection(
                    Projection::builder()
                        .projection_type(ProjectionType::All)
                        .build(),
                )
                .build()
                .unwrap(),
        )
        .send()
        .await;
    if let Err(e) = created {
        eprintln!("skipped: DynamoDB Local doesn't support multi-attribute GSI keys: {e:?}");
        return Ok(());
    }
    let projects = ProjectClient::new(&Config::new(ddb), &table)?;
    let p = |id: &str, owner: &str, due: &str| ProjectInput {
        org_id: "O1".into(),
        project_id: id.into(),
        owner: some(owner),
        due: some(due),
        ..Default::default()
    };
    for input in [
        p("P1", "M1", "2026-02-01"),
        p("P2", "M1", "2026-05-01"),
        p("P3", "M2", "2026-05-01"),
    ] {
        projects.put(input).exec().await?;
    }
    let got = projects
        .by_owner("O1", "M1")
        .into_query()
        .exec()
        .await?
        .items;
    assert_eq!(got.len(), 2, "by owner = {got:?}");
    let got = projects
        .by_owner("O1", "M1")
        .due()
        .gt("2026-03-01")
        .exec()
        .await?
        .items;
    assert_eq!(
        got.iter()
            .map(|p| p.project_id.as_str())
            .collect::<Vec<_>>(),
        ["P2"],
        "by owner gt"
    );
    Ok(())
}
