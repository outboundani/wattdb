//! Entities matching the recorded vectors' schemas.

#![allow(dead_code)]

pub const TASKAPP: wattdb::Service = wattdb::Service::new("taskapp");

/// The "multi" vectors' member: batches, transactions and collections.
pub mod member {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[wattdb(rename_all = "lowercase")]
    pub enum Status {
        #[default]
        Active,
        Away,
        /// A value another writer stored.
        #[wattdb(other)]
        Other(String),
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

/// The "multi" vectors' project, with a composite index.
pub mod project {
    use wattdb::{Entity, Enum};

    #[derive(Enum, Clone, Copy, Debug, Default, PartialEq, Eq)]
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

/// The collections of the "multi" vectors' service.
pub mod taskapp {
    use wattdb::Collection;

    use super::member::Member;
    use super::project::Project;

    #[derive(Collection, Debug, Default, PartialEq)]
    #[wattdb(collection = "team")]
    pub struct Team {
        pub members: Vec<Member>,
        pub projects: Vec<Project>,
    }

    #[derive(Collection, Debug, Default, PartialEq)]
    #[wattdb(collection = "board")]
    pub struct Board {
        pub members: Vec<Member>,
        pub projects: Vec<Project>,
    }

    #[derive(Collection, Debug, Default, PartialEq)]
    #[wattdb(collection = "org")]
    pub struct Org {
        pub members: Vec<Member>,
        pub projects: Vec<Project>,
    }

    #[derive(Collection, Debug, Default, PartialEq)]
    #[wattdb(collection = "orgMembers")]
    pub struct OrgMembers {
        pub members: Vec<Member>,
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The "pipeline" vector's doc, with its registry callbacks written in Rust.
pub mod doc {
    use serde::{Deserialize, Serialize};
    use wattdb::{Entity, Enum, Map};

    #[derive(Enum, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    #[wattdb(rename_all = "lowercase")]
    pub enum Kind {
        #[default]
        Note,
        Task,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Meta {
        #[wattdb(default = "web")]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub source: Option<String>,
        #[wattdb(field = "r")]
        #[serde(rename = "rank", skip_serializing_if = "Option::is_none")]
        pub rank: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub inner: Option<Inner>,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Inner {
        #[wattdb(field = "d")]
        #[serde(rename = "deep", skip_serializing_if = "Option::is_none")]
        pub deep: Option<String>,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Opts {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub a: Option<String>,
    }

    #[derive(Map, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Step {
        #[wattdb(field = "n", required)]
        #[serde(rename = "name")]
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub ok: Option<bool>,
    }

    fn upper(v: Option<String>, _: &DocPartial) -> Option<String> {
        v.map(|s| s.to_uppercase())
    }

    fn got(v: Option<String>, _: &DocPartial) -> Option<String> {
        v.map(|s| format!("got:{s}"))
    }

    fn summary(_: Option<String>, d: &DocPartial) -> Option<String> {
        let kind = d.kind.as_ref().map(Kind::as_str).unwrap_or_default();
        Some(format!("{kind}/{}", d.status.as_deref().unwrap_or_default()))
    }

    fn note(v: Option<&String>) -> Result<(), &'static str> {
        match v {
            Some(s) if super::utf16_len(s) < 2 => Err("Invalid value provided"),
            _ => Ok(()),
        }
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    #[wattdb(entity = "doc", version = "1", service = super::TASKAPP, rename_all = "camelCase", serde)]
    #[wattdb(index(access_pattern = "primary", pk(field = "pk", composite(doc_id)), sk(field = "sk", composite())))]
    pub struct Doc {
        #[wattdb(required)]
        pub doc_id: String,
        #[wattdb(set = upper, validate_regex = "^[A-Z]+$")]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub code: Option<String>,
        #[wattdb(field = "t", json = "title", get = got)]
        #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
        pub title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub count: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub done: Option<bool>,
        #[wattdb(default = Kind::Note)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub kind: Option<Kind>,
        #[wattdb(required, default = "open")]
        pub status: String,
        #[wattdb(as_set)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tags: Option<Vec<String>>,
        #[wattdb(as_set)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub scores: Option<Vec<f64>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub meta: Option<Meta>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub opts: Option<Opts>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub steps: Option<Vec<Step>>,
        #[wattdb(items(validate = note))]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub notes: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub blob: Option<serde_json::Value>,
        #[wattdb(watch_all, set = summary, get = upper)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub summary: Option<String>,
        #[wattdb(hidden)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub secret: Option<String>,
    }
}

/// The "callbacks" vector's task.
pub mod task {
    use serde::Serialize;
    use wattdb::Entity;

    fn title(v: Option<&String>) -> Result<(), &'static str> {
        match v {
            Some(s) if super::utf16_len(s) < 3 => Err("Invalid value provided"),
            _ => Ok(()),
        }
    }

    fn lower(v: Option<String>, _: &TaskPartial) -> Option<String> {
        v.map(|s| s.to_lowercase())
    }

    fn upper(v: Option<String>, _: &TaskPartial) -> Option<String> {
        v.map(|s| s.to_uppercase())
    }

    fn search(_: Option<String>, t: &TaskPartial) -> Option<String> {
        Some(format!("{}|{}", t.title.as_deref().unwrap_or_default(), t.status.as_deref().unwrap_or_default()))
    }

    fn assigned(t: &TaskPartial) -> bool {
        t.assignee.is_some()
    }

    #[derive(Entity, Clone, Debug, Default, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    #[wattdb(entity = "task", version = "1", service = super::TASKAPP, rename_all = "camelCase", serde)]
    #[wattdb(index(access_pattern = "primary", pk(field = "pk", composite(task_id)), sk(field = "sk", composite())))]
    #[wattdb(index(access_pattern = "byAssignee", index = "gsi1", condition = assigned,
        pk(field = "gsi1pk", composite(assignee)), sk(field = "gsi1sk", composite())))]
    pub struct Task {
        #[wattdb(required)]
        pub task_id: String,
        #[wattdb(required, validate = title)]
        pub title: String,
        #[wattdb(validate_regex = r"^[A-Z]{2}-\d+$")]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub code: Option<String>,
        #[wattdb(set = lower, get = upper)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub status: Option<String>,
        #[wattdb(watch(title, status), set = search)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub search: Option<String>,
        #[wattdb(hidden)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub secret: Option<String>,
        #[wattdb(read_only)]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub created_by: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub assignee: Option<String>,
    }
}
