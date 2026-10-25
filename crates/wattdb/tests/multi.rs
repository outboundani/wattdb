//! Replays the "multi" vectors through derived entities and the real SDK
//! client, comparing the requests on the wire with ElectroDB's.

mod common;
mod fixtures;

use aws_sdk_dynamodb::types::AttributeValue;
use common::{Recorded, check_requests, config, list, recorded, result};
use fixtures::member::{self, Member, MemberClient, MemberInput, MemberKey, MemberPartial};
use fixtures::project::{self, Project, ProjectClient, ProjectInput, ProjectKey, ProjectPartial};
use fixtures::taskapp::{
    Board, BoardClient, BoardKey, Org, OrgClient, OrgKey, OrgMembers, OrgMembersClient, OrgMembersKey, Team,
    TeamClient, TeamKey,
};
use wattdb::{Config, Error, Item, Strict, Value};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn load(name: &str) -> (Recorded, Config, common::Fake) {
    let rc = recorded("multi", name);
    let (cfg, fake) = config(&rc);
    (rc, cfg, fake)
}

fn members(cfg: &Config) -> MemberClient {
    MemberClient::new(cfg, "table").unwrap()
}

fn projects(cfg: &Config) -> ProjectClient {
    ProjectClient::new(cfg, "table").unwrap()
}

fn s(v: &str) -> AttributeValue {
    AttributeValue::S(v.into())
}

fn some(v: &str) -> Option<String> {
    Some(v.into())
}

fn data(rc: &Recorded) -> wattdb::Attrs {
    match result(rc).remove("data") {
        Some(Value::Map(m)) => m,
        v => panic!("data isn't a map: {v:?}"),
    }
}

/// Checks a collection's requests, and that nothing is left to read.
fn check_collection<R>(rc: &Recorded, fake: &common::Fake, page: &wattdb::CollectionPage<R>) {
    check_requests(&fake.sent(), &rc.requests, &[]);
    assert!(page.cursor.is_none(), "cursor = {:?}", page.cursor);
}

fn members_of(d: &wattdb::Attrs) -> Vec<Member> {
    d.get("member").map(list).unwrap_or_default()
}

fn projects_of(d: &wattdb::Attrs) -> Vec<Project> {
    d.get("project").map(list).unwrap_or_default()
}

#[tokio::test]
async fn isolated_collection() -> Result {
    let (rc, cfg, fake) = load("isolated collection");
    let page = TeamClient::new(&cfg, "table")?.query(TeamKey { org_id: "O1".into() }).exec().await?;
    check_collection(&rc, &fake, &page);
    let d = data(&rc);
    assert_eq!(page.result, Team { members: members_of(&d), projects: projects_of(&d) });
    Ok(())
}

#[tokio::test]
async fn isolated_collection_with_a_filter() -> Result {
    let (rc, cfg, fake) = load("isolated collection with a filter");
    let page = TeamClient::new(&cfg, "table")?
        .query(TeamKey { org_id: "O1".into() })
        .where_(Member::name().eq("Ada"))
        .exec()
        .await?;
    check_collection(&rc, &fake, &page);
    let d = data(&rc);
    assert_eq!(page.result, Team { members: members_of(&d), projects: projects_of(&d) });
    Ok(())
}

#[tokio::test]
async fn clustered_collection() -> Result {
    let (rc, cfg, fake) = load("clustered collection");
    let page = BoardClient::new(&cfg, "table")?.query(BoardKey { org_id: "O1".into() }).exec().await?;
    check_collection(&rc, &fake, &page);
    let d = data(&rc);
    assert_eq!(page.result, Board { members: members_of(&d), projects: projects_of(&d) });
    Ok(())
}

#[tokio::test]
async fn clustered_collection_by_status() -> Result {
    let (rc, cfg, fake) = load("clustered collection by status");
    let page = BoardClient::new(&cfg, "table")?
        .query(BoardKey { org_id: "O1".into() })
        .status()
        .eq(member::Status::Away)
        .exec()
        .await?;
    check_collection(&rc, &fake, &page);
    let d = data(&rc);
    assert_eq!(page.result, Board { members: members_of(&d), projects: projects_of(&d) });
    Ok(())
}

#[tokio::test]
async fn clustered_collection_gt() -> Result {
    let (rc, cfg, fake) = load("clustered collection gt");
    let page = BoardClient::new(&cfg, "table")?
        .query(BoardKey { org_id: "O1".into() })
        .status()
        .gt(member::Status::Active)
        .exec()
        .await?;
    check_collection(&rc, &fake, &page);
    let d = data(&rc);
    assert_eq!(page.result, Board { members: members_of(&d), projects: projects_of(&d) });
    Ok(())
}

#[tokio::test]
async fn sub_collection_parent() -> Result {
    let (rc, cfg, fake) = load("sub-collection parent");
    let page = OrgClient::new(&cfg, "table")?.query(OrgKey { org_id: "O1".into() }).exec().await?;
    check_collection(&rc, &fake, &page);
    let d = data(&rc);
    assert_eq!(page.result, Org { members: members_of(&d), projects: projects_of(&d) });
    Ok(())
}

#[tokio::test]
async fn sub_collection_child() -> Result {
    let (rc, cfg, fake) = load("sub-collection child");
    let page = OrgMembersClient::new(&cfg, "table")?.query(OrgMembersKey { org_id: "O1".into() }).exec().await?;
    check_collection(&rc, &fake, &page);
    assert_eq!(page.result, OrgMembers { members: members_of(&data(&rc)) });
    Ok(())
}

#[tokio::test]
async fn collection_pages() -> Result {
    let (rc, cfg, fake) = load("collection pages");
    let page = TeamClient::new(&cfg, "table")?.query(TeamKey { org_id: "O1".into() }).all_pages().exec().await?;
    check_collection(&rc, &fake, &page);
    let d = data(&rc);
    assert_eq!(page.result, Team { members: members_of(&d), projects: projects_of(&d) });
    Ok(())
}

#[tokio::test]
async fn write_transaction() -> Result {
    let (rc, cfg, fake) = load("write transaction");
    let (m, p) = (members(&cfg), projects(&cfg));
    wattdb::tx_write(
        &cfg,
        [
            m.put(MemberInput { org_id: "O1".into(), member_id: "M1".into(), name: some("Ada"), ..Default::default() })
                .tx_op(),
            p.update(ProjectKey { org_id: "O1".into(), project_id: "P1".into() })
                .set(Project::name(), "Zeus".to_string())
                .where_(Project::status().eq(project::Status::Active))
                .tx_op(),
            m.delete(MemberKey { org_id: "O1".into(), member_id: "M2".into() }).tx_op(),
            m.check(MemberKey { org_id: "O1".into(), member_id: "M3".into() }).where_(Member::name().exists()).tx_op(),
            p.create(ProjectInput { org_id: "O1".into(), project_id: "P2".into(), name: some("Hera"), ..Default::default() })
                .tx_op_return_old(),
        ],
    )
    .token("req-1")
    .exec()
    .await?;
    check_requests(&fake.sent(), &rc.requests, &[]);
    Ok(())
}

#[tokio::test]
async fn canceled_write_transaction() -> Result {
    let (rc, cfg, fake) = load("canceled write transaction");
    let (m, p) = (members(&cfg), projects(&cfg));
    let err = wattdb::tx_write(
        &cfg,
        [
            m.put(MemberInput { org_id: "O1".into(), member_id: "M1".into(), ..Default::default() }).tx_op(),
            p.create(ProjectInput { org_id: "O1".into(), project_id: "P1".into(), ..Default::default() })
                .tx_op_return_old(),
        ],
    )
    .exec()
    .await
    .unwrap_err();
    check_requests(&fake.sent(), &rc.requests, &["ClientRequestToken"]);
    let Error::TxCanceled(tce) = err else { panic!("err = {err:?}, want TxCanceled") };
    let r = &tce.reasons;
    assert_eq!(r.len(), 2);
    assert!(!r[0].rejected && r[0].code == "None", "{:?}", r[0]);
    assert!(r[1].rejected && r[1].code == "ConditionalCheckFailed", "{:?}", r[1]);
    let want = Project {
        org_id: "O1".into(),
        project_id: "P1".into(),
        name: some("Apollo"),
        status: project::Status::Active,
        owner: some("M1"),
        due: some("2026-03-01"),
    };
    assert_eq!(r[1].item::<Project>(), Some(&want));
    assert_eq!(r[0].item::<Project>(), None);
    Ok(())
}

#[tokio::test]
async fn get_transaction() -> Result {
    let (rc, cfg, fake) = load("get transaction");
    let (m, p) = (members(&cfg), projects(&cfg));
    let mut tx = wattdb::TxGet::new(&cfg);
    let a = tx.add(m.get(MemberKey { org_id: "O1".into(), member_id: "M1".into() }).tx_get());
    let b = tx.add(p.get(ProjectKey { org_id: "O1".into(), project_id: "P9".into() }).tx_get());
    let out = tx.exec().await?;
    check_requests(&fake.sent(), &rc.requests, &[]);
    let want = Member {
        org_id: "O1".into(),
        member_id: "M1".into(),
        name: some("Ada"),
        status: member::Status::Active,
        visits: Some(3),
    };
    assert_eq!(out.get(a)?, Some(want));
    assert_eq!(out.get(b)?, None);
    Ok(())
}

fn keys(ids: &[&str]) -> Vec<MemberKey> {
    ids.iter().map(|id| MemberKey { org_id: "O1".into(), member_id: id.to_string() }).collect()
}

#[tokio::test]
async fn batch_get() -> Result {
    let (rc, cfg, fake) = load("batch get");
    let out = members(&cfg).batch_get(keys(&["M1", "M2", "M3"])).exec().await?;
    check_requests(&fake.sent(), &rc.requests, &[]);
    let want: Vec<Member> = list(result(&rc).get("data").unwrap());
    assert_eq!(out.found, want);
    assert_eq!(out.unprocessed, keys(&["M3"]));
    Ok(())
}

#[tokio::test]
async fn batch_get_preserving_order() -> Result {
    let (rc, cfg, fake) = load("batch get preserving order");
    let out = members(&cfg).batch_get(keys(&["M1", "M3", "M2"])).preserve_order().exec().await?;
    check_requests(&fake.sent(), &rc.requests, &[]);
    let ids: Vec<_> = out.items.iter().map(|m| m.as_ref().map(|m| m.member_id.as_str())).collect();
    assert_eq!(ids, [Some("M1"), None, Some("M2")]);
    assert!(out.unprocessed.is_empty());
    Ok(())
}

#[tokio::test]
async fn batch_get_of_101_keys() -> Result {
    let (rc, cfg, _) = load("batch get of 101 keys");
    let ks: Vec<_> = (0..101).map(|i| MemberKey { org_id: "O1".into(), member_id: format!("M{i}") }).collect();
    let ins = members(&cfg).batch_get(ks).params()?;
    assert_eq!(ins.len(), rc.requests.len());
    let n = |i: usize| ins[i].request_items().unwrap()["table"].keys().len();
    assert_eq!((n(0), n(1)), (100, 1));
    Ok(())
}

#[tokio::test]
async fn batch_put() -> Result {
    let (rc, cfg, fake) = load("batch put");
    let ins = vec![
        MemberInput { org_id: "O1".into(), member_id: "M1".into(), name: some("Ada"), ..Default::default() },
        MemberInput { org_id: "O1".into(), member_id: "M2".into(), ..Default::default() },
    ];
    let unprocessed = members(&cfg).batch_put(ins.clone()).exec().await?;
    check_requests(&fake.sent(), &rc.requests, &[]);
    assert_eq!(unprocessed, ins[1..]);
    Ok(())
}

#[tokio::test]
async fn batch_put_of_26_items() -> Result {
    let (rc, cfg, _) = load("batch put of 26 items");
    let ins: Vec<_> = (0..26)
        .map(|i| MemberInput { org_id: "O1".into(), member_id: format!("M{i}"), ..Default::default() })
        .collect();
    let params = members(&cfg).batch_put(ins).params()?;
    assert_eq!(params.len(), rc.requests.len());
    let n = |i: usize| params[i].request_items().unwrap()["table"].len();
    assert_eq!((n(0), n(1)), (25, 1));
    Ok(())
}

#[tokio::test]
async fn batch_delete() -> Result {
    let (rc, cfg, fake) = load("batch delete");
    let unprocessed = members(&cfg).batch_delete(keys(&["M1", "M2"])).exec().await?;
    check_requests(&fake.sent(), &rc.requests, &[]);
    assert_eq!(unprocessed, keys(&["M2"]));
    Ok(())
}

#[tokio::test]
async fn batch_put_with_an_invalid_item() -> Result {
    let (_, cfg, fake) = load("batch put with an invalid item");
    let status = Some(member::Status::Other("gone".into()));
    let err = members(&cfg)
        .batch_put([MemberInput { org_id: "O1".into(), member_id: "M1".into(), status, ..Default::default() }])
        .exec()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "err = {err:?}");
    assert!(fake.sent().is_empty());
    Ok(())
}

#[tokio::test]
async fn composite_index() -> Result {
    type Run = fn(&ProjectClient) -> wattdb::Query<Project>;
    let cases: [(&str, Run); 4] = [
        ("composite index query", |p| p.by_owner("O1", "M1").into_query()),
        ("composite index query with sk", |p| p.by_owner("O1", "M1").due().eq("2026-03-01").into_query()),
        ("composite index between", |p| p.by_owner("O1", "M1").due().between("2026-01-01", "2026-06-01")),
        ("composite index gt", |p| p.by_owner("O1", "M1").due().gt("2026-01-01")),
    ];
    for (name, run) in cases {
        let (rc, cfg, fake) = load(name);
        run(&projects(&cfg)).exec().await?;
        check_requests(&fake.sent(), &rc.requests, &[]);
    }
    Ok(())
}

#[tokio::test]
async fn find() -> Result {
    let cases: [(&str, fn(&Config) -> wattdb::Find<Member>); 5] = [
        ("find by primary key", |c| {
            members(c).find(MemberPartial { org_id: some("O1"), member_id: some("M1"), ..Default::default() })
        }),
        ("find by pk only", |c| members(c).find(MemberPartial { org_id: some("O1"), ..Default::default() })),
        ("find with no index", |c| members(c).find(MemberPartial { visits: Some(3), ..Default::default() })),
        ("match", |c| {
            members(c).matching(MemberPartial {
                org_id: some("O1"),
                name: some("Ada"),
                visits: Some(3),
                ..Default::default()
            })
        }),
        ("match with no index", |c| members(c).matching(MemberPartial { name: some("Ada"), ..Default::default() })),
    ];
    for (name, run) in cases {
        let (rc, cfg, fake) = load(name);
        run(&cfg).exec().await?;
        check_requests(&fake.sent(), &rc.requests, &[]);
    }
    let (rc, cfg, fake) = load("find by a gsi");
    projects(&cfg).find(ProjectPartial { org_id: some("O1"), name: some("Apollo"), ..Default::default() }).exec().await?;
    check_requests(&fake.sent(), &rc.requests, &[]);
    Ok(())
}

#[test]
fn conversions() -> Result {
    let (_, cfg, _) = load("composite to keys");
    let conv = members(&cfg).conv();
    let keys = conv.keys_from_composite(
        &MemberPartial { org_id: some("O1"), member_id: some("M1"), ..Default::default() },
        Strict::None,
    )?;
    assert_eq!(keys["gsi2sk"], s("$board#status_"));
    let item: Item = [("pk".into(), s("$taskapp#orgid_o1")), ("sk".into(), s("$member_1#memberid_m1"))].into();
    let cur = conv.cursor_from_keys(&item)?.expect("a cursor");
    assert!(conv.composite_from_cursor(&cur).is_err(), "accepted keys missing gsi1, as ElectroDB doesn't");
    let board = conv.board();
    let comp = board.composite_from_keys(
        &[
            ("gsi2pk".into(), s("$taskapp#orgid_o1")),
            ("gsi2sk".into(), s("$board#status_away#memberid_m2#member_1")),
            ("pk".into(), s("$taskapp#orgid_o1")),
            ("sk".into(), s("$member_1#memberid_m2")),
        ]
        .into(),
    )?;
    assert_eq!(
        comp,
        MemberPartial { org_id: some("o1"), member_id: some("m2"), status: Some(member::Status::Away), ..Default::default() }
    );
    let partial = MemberPartial { org_id: some("O1"), status: Some(member::Status::Away), ..Default::default() };
    assert!(board.keys_from_composite(&partial, Strict::All).is_err(), "strict all accepted a partial composite");
    Ok(())
}

#[test]
fn parse() -> Result {
    let (_, cfg, _) = load("parse");
    let m = members(&cfg);
    let mut item: Item = [
        ("orgId", "O1"),
        ("memberId", "M1"),
        ("name", "Ada"),
        ("status", "active"),
        ("pk", "$taskapp#orgid_o1"),
        ("sk", "$member_1#memberid_m1"),
        ("__edb_e__", "other"),
        ("__edb_v__", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), s(v)))
    .collect();
    let got = m.parse(&item)?.expect("an item");
    assert_eq!(got.member_id, "M1");
    item.insert("pk".into(), s("x"));
    assert_eq!(m.parse(&item)?, None);
    Ok(())
}
