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
