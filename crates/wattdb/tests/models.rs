//! Each vector entity written with the derive compiles to the same model as
//! the ElectroDB schema recorded in its vector.

mod fixtures;

use std::fmt::Write;

use wattdb::Entity;
use wattdb::engine::Model;
use wattdb::engine::model::{Attribute, Index};

const TASKAPP: wattdb::Service = wattdb::Service::new("taskapp");

mod attr_pk {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Debug, Default, PartialEq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Plan {
        #[default]
        Free,
        Pro,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "team", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(access_pattern = "primary", pk(field = "teamId", composite(team_id))))]
    pub struct Team {
        #[wattdb(required)]
        pub team_id: String,
        #[wattdb(required)]
        pub name: String,
        #[wattdb(required)]
        pub plan: Plan,
        pub seats: Option<i64>,
    }
}

mod attr_gsi {
    use wattdb::Entity;

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "project", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "projectId", composite(project_id))
    ))]
    #[wattdb(index(
        access_pattern = "byOwner",
        index = "ownerId",
        pk(field = "ownerId", composite(owner_id))
    ))]
    pub struct Project {
        #[wattdb(required)]
        pub project_id: String,
        #[wattdb(required)]
        pub owner_id: String,
        #[wattdb(required)]
        pub title: String,
    }
}

mod numeric_gsi {
    use wattdb::Entity;

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "member", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "memberId", composite(member_id))
    ))]
    #[wattdb(index(
        access_pattern = "byTeamNumber",
        index = "teamNumber",
        pk(field = "teamNumber", composite(team_number))
    ))]
    #[wattdb(index(
        access_pattern = "byRank",
        index = "gsi1",
        pk(field = "gsi1pk", composite(rank), cast = "number")
    ))]
    #[wattdb(index(
        access_pattern = "byLevelString",
        index = "gsi2",
        pk(field = "gsi2pk", composite(level), cast = "string")
    ))]
    pub struct Member {
        #[wattdb(required)]
        pub member_id: String,
        #[wattdb(required)]
        pub team_number: i64,
        #[wattdb(required)]
        pub rank: i64,
        pub level: Option<i64>,
    }
}

mod numeric_pk {
    use wattdb::Entity;

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "counter", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "counterId", composite(counter_id))
    ))]
    pub struct Counter {
        #[wattdb(required)]
        pub counter_id: i64,
        pub payload: Option<serde_json::Value>,
    }
}

mod template_none {
    use wattdb::Entity;

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "attempt", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(
            field = "pk",
            composite(project_id, task_id, user_id),
            template = "attempt#${projectId}#${taskId}#${userId}",
            casing = "none"
        )
    ))]
    pub struct Attempt {
        #[wattdb(required)]
        pub project_id: String,
        #[wattdb(required)]
        pub task_id: String,
        #[wattdb(required)]
        pub user_id: String,
        pub count: Option<i64>,
        pub session_ids: Option<std::collections::BTreeSet<String>>,
        #[wattdb(required, default = "2026-01-02T03:04:05.678Z")]
        pub created_at: String,
    }
}

mod template_lower {
    use wattdb::Entity;

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "label", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "identifier", composite(name), template = "label#${name}")
    ))]
    pub struct Label {
        #[wattdb(required)]
        pub name: String,
        #[wattdb(required)]
        pub colors: serde_json::Value,
    }
}

mod attr_pk_sk {
    use wattdb::{Entity, Map};

    #[derive(Map, Clone, Debug, Default, PartialEq)]
    pub struct Change {
        #[wattdb(required)]
        pub field: String,
        pub from: Option<String>,
        pub to: Option<String>,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "event", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "taskId", composite(task_id)),
        sk(field = "seq", composite(seq))
    ))]
    pub struct Event {
        #[wattdb(required)]
        pub task_id: String,
        #[wattdb(required)]
        pub seq: i64,
        pub changes: Option<Vec<Change>>,
        #[wattdb(required, default = true)]
        pub visible: bool,
    }
}

mod standard {
    use wattdb::{Entity, Enum, Map};

    #[derive(Enum, Clone, Debug, Default, PartialEq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Role {
        Admin,
        #[default]
        Member,
        Guest,
    }

    #[derive(Enum, Clone, Debug, Default, PartialEq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Status {
        #[default]
        Active,
        Disabled,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq)]
    #[wattdb(rename_all = "camelCase")]
    pub struct Profile {
        pub display_name: Option<String>,
        pub links: Option<Vec<String>>,
        pub address: Option<Address>,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq)]
    pub struct Address {
        pub city: Option<String>,
        #[wattdb(field = "postalCode")]
        pub zip: Option<String>,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "user", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "pk", composite(org_id)),
        sk(field = "sk", composite(user_id))
    ))]
    #[wattdb(index(
        access_pattern = "byEmail",
        index = "gsi1",
        pk(field = "gsi1pk", composite(email)),
        sk(field = "gsi1sk", composite())
    ))]
    #[wattdb(index(
        access_pattern = "byRole",
        index = "gsi2",
        pk(field = "gsi2pk", composite(org_id)),
        sk(field = "gsi2sk", composite(role, user_id))
    ))]
    pub struct User {
        #[wattdb(required)]
        pub org_id: String,
        #[wattdb(required)]
        pub user_id: String,
        #[wattdb(required)]
        pub email: String,
        #[wattdb(required, default = Role::Member)]
        pub role: Role,
        #[wattdb(default = Status::Active)]
        pub status: Option<Status>,
        pub profile: Option<Profile>,
    }
}

mod deep_updates {
    use wattdb::{Entity, Enum, Map};

    #[derive(Enum, Clone, Debug, Default, PartialEq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Frequency {
        #[default]
        Instant,
        Daily,
        Weekly,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq)]
    pub struct Prefs {
        pub notifications: Option<Notifications>,
        pub theme: Option<String>,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq)]
    pub struct Notifications {
        pub email: Option<Email>,
        pub push: Option<Push>,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq)]
    pub struct Email {
        pub enabled: Option<bool>,
        pub frequency: Option<Frequency>,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq)]
    pub struct Push {
        pub enabled: Option<bool>,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "settings", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "pk", composite(user_id)),
        sk(field = "sk", composite())
    ))]
    pub struct Settings {
        #[wattdb(required)]
        pub user_id: String,
        pub prefs: Option<Prefs>,
        pub visits: Option<i64>,
        pub recent: Option<Vec<String>>,
        #[wattdb(as_set)]
        pub flags: Option<Vec<String>>,
        pub meta: Option<serde_json::Value>,
    }
}

mod mutations {
    use wattdb::{Entity, Enum, Map};

    #[derive(Enum, Clone, Debug, Default, PartialEq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Status {
        #[default]
        Open,
        Closed,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq)]
    pub struct Meta {
        pub a: Option<String>,
        pub b: Option<i64>,
    }

    fn owned(i: &ItemPartial) -> bool {
        i.owner_id.is_some()
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "item", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "pk", composite(org_id)),
        sk(field = "sk", composite(item_id))
    ))]
    #[wattdb(index(
        access_pattern = "byStatus",
        index = "gsi1",
        pk(field = "gsi1pk", composite(org_id)),
        sk(field = "gsi1sk", composite(status, created_at))
    ))]
    #[wattdb(index(access_pattern = "byOwner", index = "gsi2", condition = owned,
        pk(field = "gsi2pk", composite(owner_id)), sk(field = "gsi2sk", composite())))]
    pub struct Item {
        #[wattdb(required)]
        pub org_id: String,
        #[wattdb(required)]
        pub item_id: String,
        #[wattdb(required)]
        pub title: String,
        #[wattdb(required, default = Status::Open)]
        pub status: Status,
        pub created_at: Option<String>,
        pub owner_id: Option<String>,
        #[wattdb(read_only)]
        pub created_by: Option<String>,
        pub count: Option<i64>,
        #[wattdb(as_set)]
        pub tags: Option<Vec<String>>,
        pub history: Option<Vec<String>>,
        pub meta: Option<Meta>,
        pub blob: Option<serde_json::Value>,
    }
}

mod numbers {
    use wattdb::Entity;

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "metric", version = "1", service = super::TASKAPP)]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "pk", composite(name)),
        sk(field = "sk", composite(value))
    ))]
    pub struct Metric {
        #[wattdb(required)]
        pub name: String,
        #[wattdb(required)]
        pub value: f64,
    }
}

mod probes {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Debug, Default, PartialEq)]
    pub enum Status {
        #[default]
        Active,
        Off,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "User", version = "1", service = "TaskApp")]
    #[wattdb(index(
        access_pattern = "rec",
        pk(field = "pk", composite(org)),
        sk(field = "sk", composite(id))
    ))]
    #[wattdb(index(
        access_pattern = "byN",
        index = "gsi1",
        sub_collections("c1", "c2"),
        pk(field = "gsi1pk", composite(org)),
        sk(field = "gsi1sk", composite(n, b))
    ))]
    #[wattdb(index(
        access_pattern = "cl",
        index = "gsi2",
        clustered,
        collection = "cc",
        pk(field = "gsi2pk", composite(status)),
        sk(field = "gsi2sk", composite(id))
    ))]
    #[wattdb(index(
        access_pattern = "nosk",
        index = "gsi3",
        pk(field = "gsi3pk", composite(id))
    ))]
    pub struct User {
        pub id: Option<String>,
        #[wattdb(label = "O")]
        pub org: Option<String>,
        #[wattdb(padding(length = 5, char = "0"))]
        pub n: Option<f64>,
        pub b: Option<bool>,
        pub status: Option<Status>,
        #[wattdb(as_set)]
        pub tags: Option<Vec<String>>,
        #[wattdb(field = "nk")]
        pub nick: Option<String>,
    }
}

mod queries {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Debug, Default, PartialEq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Status {
        #[default]
        Open,
        Done,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "task", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(
        access_pattern = "primary",
        pk(field = "pk", composite(project_id)),
        sk(field = "sk", composite(task_id))
    ))]
    #[wattdb(index(
        access_pattern = "byStatus",
        index = "gsi1",
        pk(field = "gsi1pk", composite(project_id)),
        sk(field = "gsi1sk", composite(status, priority, created_at))
    ))]
    #[wattdb(index(
        access_pattern = "byAssignee",
        index = "gsi2",
        projection = "keys_only",
        pk(field = "gsi2pk", composite(assignee)),
        sk(field = "gsi2sk", composite(task_id))
    ))]
    #[wattdb(index(
        access_pattern = "byStatusCluster",
        index = "gsi3",
        clustered,
        pk(field = "gsi3pk", composite(status)),
        sk(field = "gsi3sk", composite(project_id, task_id))
    ))]
    #[wattdb(index(
        access_pattern = "byTitle",
        index = "gsi4",
        projection(title),
        pk(field = "gsi4pk", composite(project_id)),
        sk(field = "gsi4sk", composite(title))
    ))]
    pub struct Task {
        #[wattdb(required)]
        pub project_id: String,
        #[wattdb(required)]
        pub task_id: String,
        pub title: Option<String>,
        #[wattdb(required)]
        pub status: Status,
        #[wattdb(padding(length = 5))]
        pub priority: Option<i64>,
        pub created_at: Option<String>,
        pub assignee: Option<String>,
        #[wattdb(hidden)]
        pub secret: Option<String>,
    }
}

/// The derived model of each vector entity, keyed `<vector>/<entity>`.
fn ported() -> Vec<(&'static str, &'static Model)> {
    fn m<E: Entity>() -> &'static Model {
        E::definition().unwrap().model()
    }
    vec![
        ("01-attr-pk/team", m::<attr_pk::Team>()),
        ("02-attr-gsi/project", m::<attr_gsi::Project>()),
        ("03-numeric-gsi/member", m::<numeric_gsi::Member>()),
        ("04-numeric-pk/counter", m::<numeric_pk::Counter>()),
        ("05-template-none/attempt", m::<template_none::Attempt>()),
        ("06-template-lower/label", m::<template_lower::Label>()),
        ("07-attr-pk-sk/event", m::<attr_pk_sk::Event>()),
        ("08-standard/user", m::<standard::User>()),
        ("09-deep-updates/settings", m::<deep_updates::Settings>()),
        ("callbacks/task", m::<fixtures::task::Task>()),
        ("multi/member", m::<fixtures::member::Member>()),
        ("multi/project", m::<fixtures::project::Project>()),
        ("mutations/item", m::<mutations::Item>()),
        ("numbers/metric", m::<numbers::Metric>()),
        ("pipeline/doc", m::<fixtures::doc::Doc>()),
        ("probes/user", m::<probes::User>()),
        ("queries/task", m::<queries::Task>()),
    ]
}

#[test]
fn derived_models_match_vector_models() {
    let ported = ported();
    let mut seen = 0;
    let mut failures = Vec::new();
    for v in wattdb_vectors::load_all() {
        for (name, want) in v.models() {
            let key = format!("{}/{name}", v.name);
            let Some((_, got)) = ported.iter().find(|(k, _)| *k == key) else {
                failures.push(format!("no derive port for {key}"));
                continue;
            };
            let (g, w) = (dump(got), dump(&want));
            if g != w {
                failures.push(format!("{key}: models differ\n got: {g}\nwant: {w}"));
            }
            seen += 1;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(
        seen,
        ported.len(),
        "checked {seen} entities, ported {}",
        ported.len()
    );
}

/// Renders a model's public parts, with callbacks shown as present or
/// absent. Attribute names and paths are skipped: the derive's ElectroDB
/// name is always the field (D12), while a JS model's may differ.
fn dump(m: &Model) -> String {
    let mut b = String::new();
    write!(
        b,
        "{} {} {} {:?} [",
        m.entity, m.version, m.service, m.identifiers
    )
    .unwrap();
    for a in &m.attributes {
        attribute(&mut b, a);
    }
    b.push_str("] [");
    for ix in &m.indexes {
        index(&mut b, ix);
    }
    b.push(']');
    b
}

fn func<T>(f: &Option<T>) -> &'static str {
    if f.is_some() { "func" } else { "nofunc" }
}

fn attribute(b: &mut String, a: &Attribute) {
    write!(
        b,
        "{{field:{} ty:{:?} enum:{:?} label:{:?} required:{} hidden:{} read_only:{} padding:{:?} cast:{:?} \
         default:{} get:{} set:{} validate:{} regex:{:?} watch:{:?} watch_all:{} watched_by:{:?} \
         key_prefix:{:?} key_postfix:{:?} key_casing:{:?} root:{} items:",
        a.field,
        a.ty,
        a.enum_values,
        a.label,
        a.required,
        a.hidden,
        a.read_only,
        a.padding,
        a.cast,
        func(&a.default),
        func(&a.get),
        func(&a.set),
        func(&a.validate),
        a.validate_regex.as_ref().map(|r| r.as_str().to_string()),
        a.watch,
        a.watch_all,
        a.watched_by,
        a.key_prefix,
        a.key_postfix,
        a.key_casing,
        a.is_root,
    )
    .unwrap();
    match &a.items {
        Some(i) => attribute(b, i),
        None => b.push_str("nil"),
    }
    b.push_str(" properties:[");
    for p in &a.properties {
        attribute(b, p);
    }
    b.push_str("]} ");
}

fn index(b: &mut String, ix: &Index) {
    write!(
        b,
        "{{id:{} pattern:{} name:{} ty:{:?} scope:{:?} collections:{:?} sub:{} has_sk:{} pk:{:?} sk:{:?} \
         condition:{} projection:{:?}}} ",
        ix.id,
        ix.access_pattern,
        ix.name,
        ix.ty,
        ix.scope,
        ix.collections,
        ix.sub_collections,
        ix.has_sk,
        ix.pk,
        ix.sk,
        func(&ix.condition),
        ix.projection,
    )
    .unwrap();
}
