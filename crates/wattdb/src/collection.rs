//! Collections: several entities sharing an index, read in one query
//! (decision D43).

use std::marker::PhantomData;
use std::sync::Arc;

use aws_sdk_dynamodb::operation::query::QueryInput;
use futures::stream::{Stream, StreamExt};
use wattdb_engine::keys::QueryType;
use wattdb_engine::pager::RunResult;
use wattdb_engine::query::QueryOptions;

use crate::attr::Cond;
use crate::client::{Config, DynamoDb};
use crate::entity::{Entity, Overrides};
use crate::error::{Error, Result};
use crate::query::{CompareMode, Cursor, Encoded, Kind, Reader, SortKey, facet};
use crate::value::{Attrs, Item, Value, to_item};

/// The result struct of a collection, deriving
/// [`Collection`](derive@crate::Collection): one `Vec` per member entity.
pub trait CollectionResult: Default + Send + Sync + 'static {
    /// The collection's name.
    const NAME: &'static str;

    /// The members, in order: each entity's name and compiled schema.
    fn members() -> Result<Vec<(String, &'static wattdb_engine::Model)>>;

    /// Decodes a member's formatted item and adds it to the result.
    fn push(&mut self, member: usize, item: &Attrs) -> Result<()>;
}

/// A collection's partition-key composites; generated per entity and
/// collection.
pub trait CollectionKey {
    #[doc(hidden)]
    fn facets(&self) -> Vec<(String, Encoded)>;
}

#[doc(hidden)]
pub fn member<E: Entity>() -> Result<(String, &'static wattdb_engine::Model)> {
    let def = E::definition()?;
    Ok((def.model().entity.clone(), def.model()))
}

/// Queries one collection in a table.
pub struct Collection<R: CollectionResult> {
    cfg: Config,
    table: String,
    members: Vec<(String, &'static wattdb_engine::Model)>,
    _r: PhantomData<fn() -> R>,
}

impl<R: CollectionResult> Clone for Collection<R> {
    fn clone(&self) -> Self {
        Collection {
            cfg: self.cfg.clone(),
            table: self.table.clone(),
            members: self.members.clone(),
            _r: PhantomData,
        }
    }
}

impl<R: CollectionResult> Collection<R> {
    /// A client for the collection in `table`.
    pub fn new(cfg: &Config, table: impl Into<String>) -> Result<Self> {
        let table = table.into();
        if table.is_empty() {
            return Err(Error::Invalid("wattdb: table name is empty".into()));
        }
        let members = R::members()?;
        if members.is_empty() {
            return Err(Error::Schema(format!(
                "collection {:?} has no entities",
                R::NAME
            )));
        }
        for (alias, m) in &members {
            if m.collection_index(R::NAME).is_none() {
                return Err(Error::Schema(format!(
                    "entity {alias:?} isn't in collection {:?}",
                    R::NAME
                )));
            }
        }
        Ok(Collection {
            cfg: cfg.clone(),
            table,
            members,
            _r: PhantomData,
        })
    }

    /// Queries the collection for one partition.
    pub fn query(&self, key: &impl CollectionKey) -> CollectionQuery<R> {
        let r = Reader {
            cfg: self.cfg.clone(),
            model: self.members[0].1,
            entity: None,
            table: self.table.clone(),
            kind: Kind::Collection {
                name: R::NAME.to_string(),
                members: self.members.clone(),
            },
            facets: key.facets(),
            sort_key: None,
            conds: Vec::new(),
            opts: QueryOptions::default(),
            pages: 1,
            count: 0,
            cursor: None,
            o: Overrides::default(),
            mutate_q: Vec::new(),
            mutate_s: Vec::new(),
        };
        CollectionQuery { r, _r: PhantomData }
    }
}

/// One page of a collection query and the cursor after it.
#[derive(Debug)]
pub struct CollectionPage<R> {
    pub result: R,
    pub cursor: Option<Cursor>,
    /// The keys hydrate couldn't fetch (only with `hydrate()`).
    pub unprocessed: Vec<Item>,
}

/// A collection query being built.
pub struct CollectionQuery<R: CollectionResult> {
    r: Reader,
    _r: PhantomData<fn() -> R>,
}

fn result<R: CollectionResult>(res: &RunResult) -> Result<CollectionPage<R>> {
    let mut out = R::default();
    for (i, group) in res.groups.iter().enumerate() {
        for item in group {
            out.push(i, item)?;
        }
    }
    Ok(CollectionPage {
        result: out,
        cursor: Cursor::from_keys(&res.next)?,
        unprocessed: res.unprocessed.iter().map(to_item).collect::<Result<_>>()?,
    })
}

impl<R: CollectionResult> CollectionQuery<R> {
    #[doc(hidden)]
    pub fn key_eq(mut self, field: &str, v: Result<Value>) -> Self {
        self.r.facets.push(facet(field, v));
        self
    }

    #[doc(hidden)]
    pub fn key_compare(mut self, op: &str, field: &str, v: Result<Value>) -> Self {
        let typ = match op {
            "gt" => QueryType::Gt,
            "gte" => QueryType::Gte,
            "lt" => QueryType::Lt,
            _ => QueryType::Lte,
        };
        self.r.sort_key = Some(SortKey::Compare(typ, field.to_string(), facet(field, v).1));
        self
    }

    #[doc(hidden)]
    pub fn key_between(mut self, field: &str, lo: Result<Value>, hi: Result<Value>) -> Self {
        self.r.sort_key = Some(SortKey::Between(
            field.to_string(),
            facet(field, lo).1,
            facet(field, hi).1,
        ));
        self
    }

    #[doc(hidden)]
    pub fn key_begins(mut self, field: &str, v: Result<Value>) -> Self {
        self.r.sort_key = Some(SortKey::Begins(field.to_string(), facet(field, v).1));
        self
    }

    /// Adds a filter; attributes of any member can be used.
    pub fn where_(mut self, cond: Cond) -> Self {
        self.r.conds.push(cond);
        self
    }

    pub fn limit(mut self, n: u32) -> Self {
        self.r.opts.limit = n as i64;
        self
    }

    pub fn desc(mut self) -> Self {
        self.r.opts.order = "desc".into();
        self
    }

    /// Sends ScanIndexForward=true explicitly.
    pub fn asc(mut self) -> Self {
        self.r.opts.order = "asc".into();
        self
    }

    pub fn consistent(mut self) -> Self {
        self.r.opts.consistent = true;
        self
    }

    pub fn cursor(mut self, c: impl Into<Option<Cursor>>) -> Self {
        self.r.cursor = c.into();
        self
    }

    /// Reads up to `n` pages (default 1).
    pub fn max_pages(mut self, n: usize) -> Self {
        self.r.pages = n.max(1);
        self
    }

    /// Reads until DynamoDB has no more.
    pub fn all_pages(mut self) -> Self {
        self.r.pages = 0;
        self
    }

    /// Fetches whole items for indexes that project only keys.
    pub fn hydrate(mut self) -> Self {
        self.r.opts.hydrate = true;
        self
    }

    /// Skips the entity identifier filter and checks.
    pub fn ignore_ownership(mut self) -> Self {
        self.r.opts.ignore_ownership = true;
        self
    }

    pub fn compare(mut self, mode: CompareMode) -> Self {
        self.r.opts.compare = match mode {
            CompareMode::Keys => "keys".into(),
            CompareMode::Attributes => "attributes".into(),
        };
        self
    }

    pub fn table(mut self, name: impl Into<String>) -> Self {
        self.r.o.table = Some(name.into());
        self
    }

    pub fn client(mut self, client: impl DynamoDb) -> Self {
        self.r.o.client = Some(Arc::new(client));
        self
    }

    /// Edits each request before it's sent.
    pub fn mutate(mut self, f: impl Fn(&mut QueryInput) + Send + Sync + 'static) -> Self {
        self.r.mutate_q.push(Arc::new(f));
        self
    }

    /// The first request, without sending it.
    pub fn params(&self) -> Result<QueryInput> {
        let req = self.r.build(false)?;
        self.r.query_input(&req.params)
    }

    /// Runs the query, reading max_pages pages (1 by default), and returns
    /// the items grouped by entity with the cursor to continue from.
    pub async fn exec(self) -> Result<CollectionPage<R>> {
        let (_, res) = self.r.fetch().await?;
        result(&res)
    }

    /// Iterates pages until there are no more.
    pub fn pages(self) -> impl Stream<Item = Result<CollectionPage<R>>> + Send {
        self.r.pages().map(|r| r.and_then(|(_, res)| result(&res)))
    }
}
