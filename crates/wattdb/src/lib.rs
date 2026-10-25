//! A strongly typed DynamoDB library for Rust that reproduces
//! [ElectroDB](https://electrodb.dev) 3.9 byte for byte.
//!
//! Describe each entity as a struct and derive [`Entity`](derive@Entity).
//! The derive generates the entity's `Input`, `Partial` and `Key` types, a
//! client with one method per access pattern, and a descriptor per
//! attribute for conditions and updates. The items, keys, query results and
//! cursors wattdb reads and writes are the ones ElectroDB would, so Rust,
//! Go and TypeScript services can share a table.
//!
//! ```
//! use wattdb::{Config, Entity, Enum};
//!
//! #[derive(Entity, Clone, Debug)]
//! #[wattdb(entity = "task", version = "1", service = "taskapp", rename_all = "camelCase")]
//! #[wattdb(index(access_pattern = "primary", pk(field = "pk", composite(project_id)), sk(field = "sk", composite(task_id))))]
//! #[wattdb(index(access_pattern = "byAssignee", index = "gsi1", pk(field = "gsi1pk", composite(assignee)), sk(field = "gsi1sk", composite(status, task_id))))]
//! pub struct Task {
//!     #[wattdb(required)]
//!     pub project_id: String,
//!     #[wattdb(required)]
//!     pub task_id: String,
//!     #[wattdb(required, default = Status::Open)]
//!     pub status: Status,
//!     pub assignee: Option<String>,
//!     pub points: Option<i64>,
//! }
//!
//! #[derive(Enum, Clone, Debug, Default, PartialEq, Eq)]
//! #[wattdb(rename_all = "lowercase")]
//! pub enum Status { #[default] Open, Done }
//!
//! # async fn run(ddb: aws_sdk_dynamodb::Client) -> wattdb::Result<()> {
//! let cfg = Config::new(ddb);
//! let tasks = TaskClient::new(&cfg, "app-table")?;
//! tasks.create(TaskInput { project_id: "p1".into(), task_id: "t1".into(), ..Default::default() }).exec().await?;
//! tasks.update(TaskKey { project_id: "p1".into(), task_id: "t1".into() })
//!     .set(Task::status(), Status::Done)
//!     .add(Task::points(), 3)
//!     .where_(Task::status().eq(Status::Open))
//!     .exec()
//!     .await?;
//! let page = tasks.by_assignee("ada").status().eq(Status::Open).limit(50).exec().await?;
//! for task in page.items {
//!     println!("{} {}", task.task_id, task.status);
//! }
//! # Ok(()) }
//! ```

mod attr;
mod batch;
mod client;
pub mod codec;
mod collection;
mod conv;
mod entity;
mod error;
mod migrate;
mod ops;
mod query;
mod sdk;
mod tx;
mod value;

#[doc(hidden)]
pub mod __private;

pub use attr::{
    Addable, Appendable, Attr, AttrList, Cond, Deletable, Descriptor, Encoder, ListAttr,
    NumberAttr, RawArg, SetAttr, Subtractable, and, attribute_value, name, not, or, path_string,
    raw, value,
};
pub use batch::{
    Backoff, BatchGet, BatchGetOutput, BatchWrite, MAX_BATCH_WRITE, OrderedBatchGet,
    OrderedBatchGetOutput,
};
#[cfg(feature = "tracing")]
pub use client::TracingHook;
pub use client::{Call, Config, DynamoDb, Hook, Next, Request, Response};
pub use codec::{Codec, DefaultCodec, FieldType};
pub use collection::{
    Collection, CollectionKey, CollectionPage, CollectionQuery, CollectionResult,
};
pub use conv::{Conv, IndexConv, Strict};
pub use entity::{Entity, EntityDef, Table};
pub use error::{
    BoxError, CallbackError, Error, FieldError, IncompleteCompositesError, Result, SdkError,
    TxCanceledError, TxReason, ValidationError,
};
pub use migrate::migrate;
pub use ops::{
    Delete, Get, Outcome, Put, Rejectable, ReturnMode, Returning, ReturningUpdated, Update,
    UpdatedMode, Upsert,
};
pub use query::{
    CompareMode, Cursor, Find, FindParams, KeyedRead, Page, PartialRead, Query, Scan, WithKeys,
};
pub use tx::{TxCheck, TxGet, TxGetOp, TxGetOutput, TxSlot, TxWrite, TxWriteOp, tx_write};
pub use value::{
    Attrs, Item, Number, Value, from_attribute_value, from_item, to_attribute_value, to_item,
};
pub use wattdb_derive::{Collection, Entity, Enum, Map};

/// The engine wattdb runs on, for advanced uses.
pub use wattdb_engine as engine;

/// A service declared once, so a typo can't silently change keys (decision
/// D15):
///
/// ```
/// pub const TASKAPP: wattdb::Service = wattdb::Service::new("taskapp");
/// ```
///
/// and then `#[wattdb(service = TASKAPP)]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Service(&'static str);

impl Service {
    pub const fn new(name: &'static str) -> Self {
        Service(name)
    }

    pub const fn name(&self) -> &'static str {
        self.0
    }
}

impl<E: Entity> Table<E> {
    /// Reads one item.
    pub fn get(&self, key: E::Key) -> Get<E> {
        Get::new(self, &key)
    }

    /// Writes a whole item, replacing any existing one.
    pub fn put(&self, input: E::Input) -> Put<E> {
        Put::new(self, &input, false)
    }

    /// Writes a new item; a condition failure if it exists.
    pub fn create(&self, input: E::Input) -> Put<E> {
        Put::new(self, &input, true)
    }

    /// Deletes one item.
    pub fn delete(&self, key: E::Key) -> Delete<E> {
        Delete::new(self, &key, false)
    }

    /// Deletes one item; a condition failure if it doesn't exist.
    pub fn remove(&self, key: E::Key) -> Delete<E> {
        Delete::new(self, &key, true)
    }

    /// Changes attributes of one item, creating it if needed.
    pub fn update(&self, key: E::Key) -> Update<E> {
        Update::new(self, &key, false)
    }

    /// Changes attributes of one item; a condition failure if it doesn't
    /// exist.
    pub fn patch(&self, key: E::Key) -> Update<E> {
        Update::new(self, &key, true)
    }

    /// Writes the input's attributes, creating the item if needed; chain
    /// more operations before `exec`.
    pub fn upsert(&self, input: E::Input) -> Upsert<E> {
        Upsert::new(self, &input)
    }
}

/// Forwards the shared query methods to a generated query wrapper.
#[doc(hidden)]
#[macro_export]
macro_rules! __forward_query {
    ($wrapper:ident, $entity:ty) => {
        impl $wrapper {
            /// Adds a filter; filters are ANDed.
            pub fn where_(self, cond: $crate::Cond) -> Self {
                Self(self.0.where_(cond))
            }
            /// DynamoDB's per-request Limit.
            pub fn limit(self, n: u32) -> Self {
                Self(self.0.limit(n))
            }
            /// Reads the index in descending sort-key order.
            pub fn desc(self) -> Self {
                Self(self.0.desc())
            }
            /// Reads in ascending order, sent explicitly.
            pub fn asc(self) -> Self {
                Self(self.0.asc())
            }
            /// Makes the reads strongly consistent.
            pub fn consistent(self) -> Self {
                Self(self.0.consistent())
            }
            /// Resumes after a previous page.
            pub fn cursor(
                self,
                c: impl ::core::convert::Into<::core::option::Option<$crate::Cursor>>,
            ) -> Self {
                Self(self.0.cursor(c))
            }
            /// Makes `exec` read up to `n` pages (default 1).
            pub fn max_pages(self, n: usize) -> Self {
                Self(self.0.max_pages(n))
            }
            /// Makes `exec` read every page.
            pub fn all_pages(self) -> Self {
                Self(self.0.all_pages())
            }
            /// Makes `exec` stop after `n` items.
            pub fn count(self, n: usize) -> Self {
                Self(self.0.count(n))
            }
            /// Fetches the full items behind a keys-only or partial projection.
            pub fn hydrate(self) -> Self {
                Self(self.0.hydrate())
            }
            /// Skips the entity identifier checks.
            pub fn ignore_ownership(self) -> Self {
                Self(self.0.ignore_ownership())
            }
            /// How sort-key comparisons treat composites.
            pub fn compare(self, mode: $crate::CompareMode) -> Self {
                Self(self.0.compare(mode))
            }
            /// Overrides the table for this call.
            pub fn table(self, name: impl ::core::convert::Into<::std::string::String>) -> Self {
                Self(self.0.table(name))
            }
            /// Overrides the DynamoDB client for this call.
            pub fn client(self, client: impl $crate::DynamoDb) -> Self {
                Self(self.0.client(client))
            }
            /// Edits each request before it's sent.
            pub fn mutate(
                self,
                f: impl Fn(&mut $crate::__private::QueryInput) + Send + Sync + 'static,
            ) -> Self {
                Self(self.0.mutate(f))
            }
            /// The first request, without sending it.
            pub fn params(&self) -> $crate::Result<$crate::__private::QueryInput> {
                self.0.params()
            }
            /// Reads only the given attributes, returning partials.
            pub fn attributes(
                self,
                attrs: impl $crate::AttrList<$entity>,
            ) -> $crate::PartialRead<$entity> {
                self.0.attributes(attrs)
            }
            /// Returns items with their stored key attributes.
            pub fn include_keys(self) -> $crate::KeyedRead<$entity> {
                self.0.include_keys()
            }
            /// Reads the items, with a cursor to continue from.
            pub async fn exec(self) -> $crate::Result<$crate::Page<$entity>> {
                self.0.exec().await
            }
            /// Iterates pages until there are no more.
            pub fn pages(
                self,
            ) -> impl $crate::__private::futures::Stream<
                Item = $crate::Result<$crate::Page<$entity>>,
            > + Send {
                self.0.pages()
            }
            /// Iterates items until there are no more.
            pub fn all(
                self,
            ) -> impl $crate::__private::futures::Stream<Item = $crate::Result<$entity>> + Send
            {
                self.0.all()
            }
            /// Returns DynamoDB's responses as they are.
            pub async fn raw(
                self,
            ) -> $crate::Result<::std::vec::Vec<$crate::__private::QueryOutput>> {
                self.0.raw().await
            }
            /// The underlying query.
            pub fn into_query(self) -> $crate::Query<$entity> {
                self.0
            }
        }

        impl ::core::convert::From<$wrapper> for $crate::Query<$entity> {
            fn from(q: $wrapper) -> Self {
                q.0
            }
        }
    };
}

/// Forwards the shared collection-query methods to a generated wrapper.
#[doc(hidden)]
#[macro_export]
macro_rules! __forward_collection_query {
    ($wrapper:ident) => {
        impl<R: $crate::CollectionResult> $wrapper<R> {
            /// Adds a filter; attributes of any member can be used.
            pub fn where_(self, cond: $crate::Cond) -> Self {
                Self(self.0.where_(cond))
            }
            pub fn limit(self, n: u32) -> Self {
                Self(self.0.limit(n))
            }
            pub fn desc(self) -> Self {
                Self(self.0.desc())
            }
            pub fn asc(self) -> Self {
                Self(self.0.asc())
            }
            pub fn consistent(self) -> Self {
                Self(self.0.consistent())
            }
            pub fn cursor(
                self,
                c: impl ::core::convert::Into<::core::option::Option<$crate::Cursor>>,
            ) -> Self {
                Self(self.0.cursor(c))
            }
            pub fn max_pages(self, n: usize) -> Self {
                Self(self.0.max_pages(n))
            }
            pub fn all_pages(self) -> Self {
                Self(self.0.all_pages())
            }
            pub fn hydrate(self) -> Self {
                Self(self.0.hydrate())
            }
            pub fn ignore_ownership(self) -> Self {
                Self(self.0.ignore_ownership())
            }
            pub fn compare(self, mode: $crate::CompareMode) -> Self {
                Self(self.0.compare(mode))
            }
            pub fn table(self, name: impl ::core::convert::Into<::std::string::String>) -> Self {
                Self(self.0.table(name))
            }
            pub fn client(self, client: impl $crate::DynamoDb) -> Self {
                Self(self.0.client(client))
            }
            pub fn mutate(
                self,
                f: impl Fn(&mut $crate::__private::QueryInput) + Send + Sync + 'static,
            ) -> Self {
                Self(self.0.mutate(f))
            }
            pub fn params(&self) -> $crate::Result<$crate::__private::QueryInput> {
                self.0.params()
            }
            /// Runs the query, returning the items grouped by entity.
            pub async fn exec(self) -> $crate::Result<$crate::CollectionPage<R>> {
                self.0.exec().await
            }
            /// Iterates pages until there are no more.
            pub fn pages(
                self,
            ) -> impl $crate::__private::futures::Stream<
                Item = $crate::Result<$crate::CollectionPage<R>>,
            > + Send {
                self.0.pages()
            }
            /// The underlying query.
            pub fn into_query(self) -> $crate::CollectionQuery<R> {
                self.0
            }
        }
    };
}
