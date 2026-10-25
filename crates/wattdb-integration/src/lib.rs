//! wattdb's DynamoDB Local tests (decision D63). The tests need the
//! `integration` feature and either Docker or a running DynamoDB Local:
//!
//! ```sh
//! cargo test -p wattdb-integration --features integration
//! WATTDB_DYNAMODB_ENDPOINT=http://localhost:8000 cargo test -p wattdb-integration --features integration
//! ```
//!
//! The entities match the recorded vectors', so items wattdb writes can be
//! compared with the items ElectroDB recorded.

#[cfg(feature = "integration")]
pub mod local;

pub const TASKAPP: wattdb::Service = wattdb::Service::new("taskapp");

/// The "mutations" vector entity.
pub mod item {
    use serde::{Deserialize, Serialize};
    use wattdb::{Entity, Enum, Map};

    #[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
    #[wattdb(rename_all = "lowercase", serde)]
    pub enum Status {
        #[default]
        Open,
        Closed,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Meta {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub a: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub b: Option<i64>,
    }

    fn owned(i: &ItemPartial) -> bool {
        i.owner_id.is_some()
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "item", version = "1", service = super::TASKAPP, rename_all = "camelCase", serde)]
    #[wattdb(index(access_pattern = "primary", pk(field = "pk", composite(org_id)), sk(field = "sk", composite(item_id))))]
    #[wattdb(index(access_pattern = "byStatus", index = "gsi1", pk(field = "gsi1pk", composite(org_id)),
        sk(field = "gsi1sk", composite(status, created_at))))]
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

/// The "multi" vector entities: batches, transactions, collections and a
/// composite index.
pub mod member {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Status {
        #[default]
        Active,
        Away,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "member", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(access_pattern = "primary", pk(field = "pk", composite(org_id)), sk(field = "sk", composite(member_id))))]
    #[wattdb(index(access_pattern = "team", index = "gsi1", collection = "team",
        pk(field = "gsi1pk", composite(org_id)), sk(field = "gsi1sk", composite(member_id))))]
    #[wattdb(index(access_pattern = "board", index = "gsi2", clustered, collection = "board",
        pk(field = "gsi2pk", composite(org_id)), sk(field = "gsi2sk", composite(status, member_id))))]
    #[wattdb(index(access_pattern = "org", index = "gsi3", sub_collections("org", "orgMembers"),
        pk(field = "gsi3pk", composite(org_id)), sk(field = "gsi3sk", composite(name))))]
    pub struct Member {
        #[wattdb(required)]
        pub org_id: String,
        #[wattdb(required)]
        pub member_id: String,
        pub name: Option<String>,
        #[wattdb(required, default = Status::Active)]
        pub status: Status,
        pub visits: Option<i64>,
    }
}

pub mod project {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Status {
        #[default]
        Active,
        Away,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "project", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(access_pattern = "primary", pk(field = "pk", composite(org_id)), sk(field = "sk", composite(project_id))))]
    #[wattdb(index(access_pattern = "team", index = "gsi1", collection = "team",
        pk(field = "gsi1pk", composite(org_id)), sk(field = "gsi1sk", composite(project_id))))]
    #[wattdb(index(access_pattern = "board", index = "gsi2", clustered, collection = "board",
        pk(field = "gsi2pk", composite(org_id)), sk(field = "gsi2sk", composite(status, project_id))))]
    #[wattdb(index(access_pattern = "org", index = "gsi3", sub_collections("org", "orgProjects"),
        pk(field = "gsi3pk", composite(org_id)), sk(field = "gsi3sk", composite(name))))]
    #[wattdb(index(access_pattern = "byOwner", index = "gsi4", composite,
        pk(composite(org_id, owner)), sk(composite(due, project_id))))]
    pub struct Project {
        #[wattdb(required)]
        pub org_id: String,
        #[wattdb(required)]
        pub project_id: String,
        pub name: Option<String>,
        #[wattdb(required, default = Status::Active)]
        pub status: Status,
        pub owner: Option<String>,
        pub due: Option<String>,
    }
}

/// The member and project collections.
pub mod taskapp {
    use wattdb::Collection;

    use super::member::Member;
    use super::project::Project;

    #[derive(Collection, Debug, Default)]
    #[wattdb(collection = "team")]
    pub struct Team {
        pub members: Vec<Member>,
        pub projects: Vec<Project>,
    }

    #[derive(Collection, Debug, Default)]
    #[wattdb(collection = "board")]
    pub struct Board {
        pub members: Vec<Member>,
        pub projects: Vec<Project>,
    }

    #[derive(Collection, Debug, Default)]
    #[wattdb(collection = "org")]
    pub struct Org {
        pub members: Vec<Member>,
        pub projects: Vec<Project>,
    }

    #[derive(Collection, Debug, Default)]
    #[wattdb(collection = "orgMembers")]
    pub struct OrgMembers {
        pub members: Vec<Member>,
    }
}

/// The "queries" vector entity.
pub mod task {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Status {
        #[default]
        Open,
        Done,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "task", version = "1", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(access_pattern = "primary", pk(field = "pk", composite(project_id)), sk(field = "sk", composite(task_id))))]
    #[wattdb(index(access_pattern = "byStatus", index = "gsi1", pk(field = "gsi1pk", composite(project_id)),
        sk(field = "gsi1sk", composite(status, priority, created_at))))]
    #[wattdb(index(access_pattern = "byAssignee", index = "gsi2", projection = "keys_only",
        pk(field = "gsi2pk", composite(assignee)), sk(field = "gsi2sk", composite(task_id))))]
    #[wattdb(index(access_pattern = "byStatusCluster", index = "gsi3", clustered,
        pk(field = "gsi3pk", composite(status)), sk(field = "gsi3sk", composite(project_id, task_id))))]
    #[wattdb(index(access_pattern = "byTitle", index = "gsi4", projection(title),
        pk(field = "gsi4pk", composite(project_id)), sk(field = "gsi4sk", composite(title))))]
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

/// Version 2 of the task: priority becomes a required enum and the
/// assignee a list of assignees.
pub mod taskv2 {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Status {
        #[default]
        Open,
        Done,
    }

    #[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Priority {
        #[default]
        Low,
        High,
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq)]
    #[wattdb(entity = "task", version = "2", service = super::TASKAPP, rename_all = "camelCase")]
    #[wattdb(index(access_pattern = "primary", pk(field = "pk", composite(project_id)), sk(field = "sk", composite(task_id))))]
    pub struct Task {
        #[wattdb(required)]
        pub project_id: String,
        #[wattdb(required)]
        pub task_id: String,
        pub title: Option<String>,
        #[wattdb(required)]
        pub status: Status,
        #[wattdb(required, default = Priority::Low)]
        pub priority: Priority,
        pub assignees: Option<Vec<String>>,
    }
}
