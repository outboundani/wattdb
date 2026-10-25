//! Queries, scans and find/match (decisions D29–D32, D38, D44).

use std::sync::Arc;

use aws_sdk_dynamodb::operation::query::{QueryInput, QueryOutput};
use aws_sdk_dynamodb::operation::scan::{ScanInput, ScanOutput};
use aws_sdk_dynamodb::types::KeysAndAttributes;
use futures::stream::{self, Stream, StreamExt};
use wattdb_engine::keys::QueryType;
use wattdb_engine::pager::{RunOptions, RunResult, Runner, encode_cursor};
use wattdb_engine::query::{Query as EngineQuery, QueryOptions, QueryRequest};
use wattdb_engine::request::{Operation, Params};
use wattdb_engine::write::Entry;

use crate::attr::{AttrList, Cond};
use crate::client::{Config, DynamoDb, Request};
use crate::entity::{Entity, Overrides, Table};
use crate::error::{Error, Result};
use crate::sdk::Sdk;
use crate::value::{Attrs, Item, Value, from_item, to_item};

/// Resumes a query or scan where a previous one stopped (decision D32).
/// It's interchangeable with ElectroDB cursors.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Cursor(String);

impl Cursor {
    pub fn new(s: impl Into<String>) -> Self {
        Cursor(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub(crate) fn from_keys(keys: &Option<Attrs>) -> Result<Option<Cursor>> {
        match keys {
            None => Ok(None),
            Some(k) => Ok(encode_cursor(k)?.map(Cursor)),
        }
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Cursor {
    fn from(s: String) -> Self {
        Cursor(s)
    }
}

impl From<&str> for Cursor {
    fn from(s: &str) -> Self {
        Cursor(s.to_string())
    }
}

impl AsRef<str> for Cursor {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// One page of results and the cursor after it.
#[derive(Clone, Debug)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Where to continue from; `None` when there's nothing more.
    pub cursor: Option<Cursor>,
    /// The keys hydrate couldn't fetch (only with `hydrate()`).
    pub unprocessed: Vec<Item>,
}

/// An item with the key attributes stored alongside it (decision D38).
#[derive(Clone, Debug)]
pub struct WithKeys<T> {
    pub item: T,
    pub keys: Item,
}

/// How sort-key comparisons treat composites (decision D51).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareMode {
    /// Compares the built key strings (the default).
    Keys,
    /// Also filters on the composite attributes.
    Attributes,
}

impl CompareMode {
    fn option(self) -> &'static str {
        match self {
            CompareMode::Keys => "keys",
            CompareMode::Attributes => "attributes",
        }
    }
}

/// A value encoded when a builder was made, kept until it runs.
pub type Encoded = std::result::Result<Value, String>;
type Mutator<I> = Arc<dyn Fn(&mut I) + Send + Sync>;

/// A key composite given to an access pattern; generated code builds them.
#[doc(hidden)]
pub fn facet(field: &str, value: Result<Value>) -> (String, Encoded) {
    (field.to_string(), value.map_err(|e| e.to_string()))
}

#[derive(Clone)]
pub(crate) enum SortKey {
    Compare(QueryType, String, Encoded),
    Between(String, Encoded, Encoded),
    Begins(String, Encoded),
}

#[derive(Clone)]
pub(crate) enum Kind {
    Query(String),
    Scan(String),
    Find,
    Match,
    Collection {
        name: String,
        members: Vec<(String, &'static wattdb_engine::Model)>,
    },
}

/// The state shared by queries, scans, finds and collection queries.
#[derive(Clone)]
pub(crate) struct Reader {
    pub cfg: Config,
    pub model: &'static wattdb_engine::Model,
    pub entity: Option<String>,
    pub table: String,
    pub kind: Kind,
    pub facets: Vec<(String, Encoded)>,
    pub sort_key: Option<SortKey>,
    pub conds: Vec<Cond>,
    pub opts: QueryOptions,
    pub pages: usize,
    pub count: usize,
    pub cursor: Option<Cursor>,
    pub o: Overrides,
    pub mutate_q: Vec<Mutator<QueryInput>>,
    pub mutate_s: Vec<Mutator<ScanInput>>,
}

fn entries(facets: &[(String, Encoded)]) -> Result<Vec<Entry>> {
    facets
        .iter()
        .map(|(k, v)| {
            v.clone()
                .map(|v| Entry::new(k.clone(), v))
                .map_err(Error::Encode)
        })
        .collect()
}

fn one(e: &Encoded) -> Result<Value> {
    e.clone().map_err(Error::Encode)
}

impl Reader {
    pub fn new<E: Entity>(t: &Table<E>, kind: Kind, facets: Vec<(String, Encoded)>) -> Self {
        Reader {
            cfg: t.config().clone(),
            model: t.model(),
            entity: Some(t.model().entity.clone()),
            table: t.name().to_string(),
            kind,
            facets,
            sort_key: None,
            conds: Vec::new(),
            opts: QueryOptions::default(),
            pages: 1,
            count: 0,
            cursor: None,
            o: Overrides::default(),
            mutate_q: Vec::new(),
            mutate_s: Vec::new(),
        }
    }

    pub fn build(&self, exec: bool) -> Result<QueryRequest<'static>> {
        let m = self.model;
        let facets = entries(&self.facets)?;
        let mut q = match &self.kind {
            Kind::Find => EngineQuery::find(m, facets),
            Kind::Match => EngineQuery::match_(m, facets),
            Kind::Scan(p) => EngineQuery::scan(m, p),
            Kind::Query(p) => EngineQuery::new(m, p, facets),
            Kind::Collection { name, members } => {
                let members = members
                    .iter()
                    .map(|(alias, model)| wattdb_engine::query::CollectionMember {
                        alias: alias.clone(),
                        model,
                    })
                    .collect();
                EngineQuery::collection(name, members, facets)?
            }
        };
        match &self.sort_key {
            None => {}
            Some(SortKey::Compare(typ, f, v)) => {
                q.compare(*typ, &[Entry::new(f.clone(), one(v)?)]);
            }
            Some(SortKey::Between(f, lo, hi)) => {
                q.between(
                    &[Entry::new(f.clone(), one(lo)?)],
                    &[Entry::new(f.clone(), one(hi)?)],
                );
            }
            Some(SortKey::Begins(f, v)) => {
                q.begins(&[Entry::new(f.clone(), one(v)?)]);
            }
        }
        for c in &self.conds {
            q.where_(&c.engine()?);
        }
        let mut opts = self.opts.clone();
        opts.exec = exec;
        if let Some(t) = &self.o.table {
            opts.table = t.clone();
        }
        Ok(q.build(&self.table, opts)?)
    }

    fn run_options(&self, pages: usize, count: usize) -> RunOptions {
        RunOptions {
            pages,
            count,
            ignore_ownership: self.opts.ignore_ownership,
            attributes: self.opts.attributes.clone(),
        }
    }

    pub fn runner(&self, scan: bool) -> SdkRunner {
        SdkRunner {
            cfg: self.cfg.clone(),
            entity: self.entity.clone(),
            o: self.o.clone(),
            scan,
            mutate_q: self.mutate_q.clone(),
            mutate_s: self.mutate_s.clone(),
            consistent: self.opts.consistent,
            raw_queries: None,
            raw_scans: None,
        }
    }

    pub fn query_input(&self, p: &Params) -> Result<QueryInput> {
        let mut input = Sdk::from(p)?.query()?;
        for f in &self.mutate_q {
            f(&mut input);
        }
        Ok(input)
    }

    pub fn scan_input(&self, p: &Params) -> Result<ScanInput> {
        let mut input = Sdk::from(p)?.scan()?;
        for f in &self.mutate_s {
            f(&mut input);
        }
        Ok(input)
    }

    /// Runs the read as `exec` does.
    pub async fn fetch(&self) -> Result<(QueryRequest<'static>, RunResult)> {
        let req = self.build(true)?;
        let start = req.start_key(self.cursor.as_ref().map(Cursor::as_str).unwrap_or(""))?;
        let mut run = self.runner(req.target == Operation::Scan);
        let res = req
            .run(start, &self.run_options(self.pages, self.count), &mut run)
            .await?;
        Ok((req, res))
    }

    /// Reads pages until DynamoDB has no more (decision D31).
    pub fn pages(self) -> impl Stream<Item = Result<(QueryRequest<'static>, RunResult)>> + Send {
        enum State {
            Init(Box<Reader>),
            Running {
                reader: Box<Reader>,
                req: Box<QueryRequest<'static>>,
                run: Box<SdkRunner>,
                start: Option<Attrs>,
            },
            Done,
        }
        stream::unfold(State::Init(Box::new(self)), |state| async move {
            let (reader, req, mut run, start) = match state {
                State::Done => return None,
                State::Init(reader) => {
                    let req = match reader.build(true) {
                        Ok(r) => r,
                        Err(e) => return Some((Err(e), State::Done)),
                    };
                    let start = match req
                        .start_key(reader.cursor.as_ref().map(Cursor::as_str).unwrap_or(""))
                    {
                        Ok(s) => s,
                        Err(e) => return Some((Err(e.into()), State::Done)),
                    };
                    let run = reader.runner(req.target == Operation::Scan);
                    (reader, Box::new(req), Box::new(run), start)
                }
                State::Running {
                    reader,
                    req,
                    run,
                    start,
                } => (reader, req, run, start),
            };
            let opt = reader.run_options(1, 0);
            match req.run(start, &opt, &mut *run).await {
                Err(e) => Some((Err(e), State::Done)),
                Ok(res) => {
                    let next = match res.next.clone() {
                        Some(n) if !n.is_empty() => State::Running {
                            reader,
                            req: req.clone(),
                            run,
                            start: Some(n),
                        },
                        _ => State::Done,
                    };
                    Some((Ok(((*req).clone(), res)), next))
                }
            }
        })
    }

    /// Reads pages without formatting, honoring max_pages/all_pages.
    pub async fn raw(&self) -> Result<(Vec<QueryOutput>, Vec<ScanOutput>)> {
        let mut r = self.clone();
        r.opts.raw = true;
        let req = r.build(true)?;
        let mut start = req.start_key(r.cursor.as_ref().map(Cursor::as_str).unwrap_or(""))?;
        let mut run = r.runner(req.target == Operation::Scan);
        run.raw_queries = Some(Vec::new());
        run.raw_scans = Some(Vec::new());
        let mut i = 0;
        while r.pages == 0 || i < r.pages {
            let (_, next) = run.page(req.with_start_key(start.as_ref())).await?;
            match next {
                Some(n) if !n.is_empty() => start = Some(n),
                _ => break,
            }
            i += 1;
        }
        Ok((
            run.raw_queries.unwrap_or_default(),
            run.raw_scans.unwrap_or_default(),
        ))
    }
}

/// Performs a read's DynamoDB calls.
pub(crate) struct SdkRunner {
    cfg: Config,
    entity: Option<String>,
    o: Overrides,
    scan: bool,
    mutate_q: Vec<Mutator<QueryInput>>,
    mutate_s: Vec<Mutator<ScanInput>>,
    consistent: bool,
    raw_queries: Option<Vec<QueryOutput>>,
    raw_scans: Option<Vec<ScanOutput>>,
}

impl Runner for SdkRunner {
    type Error = Error;

    async fn page(&mut self, params: Params) -> Result<(Vec<Attrs>, Option<Attrs>)> {
        let sdk = Sdk::from(&params)?;
        let (items, lek) = if self.scan {
            let mut input = sdk.scan()?;
            for f in &self.mutate_s {
                f(&mut input);
            }
            let out = self
                .cfg
                .send(
                    self.entity.as_deref(),
                    self.o.client.as_ref(),
                    Request::Scan(input),
                )
                .await?
                .scan()?;
            let r = (
                out.items.clone().unwrap_or_default(),
                out.last_evaluated_key.clone(),
            );
            if let Some(raw) = &mut self.raw_scans {
                raw.push(out);
            }
            r
        } else {
            let mut input = sdk.query()?;
            for f in &self.mutate_q {
                f(&mut input);
            }
            let out = self
                .cfg
                .send(
                    self.entity.as_deref(),
                    self.o.client.as_ref(),
                    Request::Query(input),
                )
                .await?
                .query()?;
            let r = (
                out.items.clone().unwrap_or_default(),
                out.last_evaluated_key.clone(),
            );
            if let Some(raw) = &mut self.raw_queries {
                raw.push(out);
            }
            r
        };
        let next = lek.filter(|k| !k.is_empty()).map(|k| from_item(&k));
        Ok((items.iter().map(from_item).collect(), next))
    }

    async fn batch_get(&mut self, table: &str, keys: Vec<Attrs>) -> Result<Vec<Attrs>> {
        let mut out = Vec::new();
        for chunk in keys.chunks(wattdb_engine::batch::MAX_BATCH_GET) {
            let ks: Vec<Item> = chunk.iter().map(to_item).collect::<Result<_>>()?;
            let ka = KeysAndAttributes::builder()
                .set_keys(Some(ks))
                .set_consistent_read(self.consistent.then_some(true))
                .build()
                .map_err(|e| Error::Invalid(e.to_string()))?;
            let input = aws_sdk_dynamodb::operation::batch_get_item::BatchGetItemInput::builder()
                .request_items(table, ka)
                .build()
                .map_err(|e| Error::Invalid(e.to_string()))?;
            let res = self
                .cfg
                .send(
                    self.entity.as_deref(),
                    self.o.client.as_ref(),
                    Request::BatchGetItem(input),
                )
                .await?
                .batch_get_item()?;
            if let Some(items) = res.responses.as_ref().and_then(|r| r.get(table)) {
                out.extend(items.iter().map(from_item));
            }
        }
        Ok(out)
    }
}

pub(crate) fn decode_page<T>(
    res: &RunResult,
    decode: &dyn Fn(&Attrs) -> Result<T>,
) -> Result<Page<T>> {
    Ok(Page {
        items: res.items.iter().map(decode).collect::<Result<_>>()?,
        cursor: Cursor::from_keys(&res.next)?,
        unprocessed: res.unprocessed.iter().map(to_item).collect::<Result<_>>()?,
    })
}

/// Turns a page stream into an item stream; hydrate's unprocessed keys come
/// after the page's items as `Error::Unprocessed`.
pub(crate) fn items_of<T: Send + 'static>(
    pages: impl Stream<Item = Result<Page<T>>> + Send,
) -> impl Stream<Item = Result<T>> + Send {
    pages.flat_map(|p| {
        let out: Vec<Result<T>> = match p {
            Err(e) => vec![Err(e)],
            Ok(page) => {
                let mut v: Vec<Result<T>> = page.items.into_iter().map(Ok).collect();
                if !page.unprocessed.is_empty() {
                    v.push(Err(Error::Unprocessed(page.unprocessed)));
                }
                v
            }
        };
        stream::iter(out)
    })
}

fn split_keys<E: Entity>(model: &wattdb_engine::Model, m: &Attrs) -> Result<WithKeys<E>> {
    let (mut attrs, mut keys) = (Attrs::new(), Attrs::new());
    for (k, v) in m {
        if model.attribute(k).is_some() {
            attrs.insert(k.clone(), v.clone());
        } else {
            keys.insert(k.clone(), v.clone());
        }
    }
    Ok(WithKeys {
        item: E::decode(&attrs)?,
        keys: to_item(&keys)?,
    })
}

/// The methods every read builder shares.
macro_rules! read_methods {
    () => {
        /// Adds a filter; filters are ANDed.
        pub fn where_(mut self, cond: Cond) -> Self {
            self.r.conds.push(cond);
            self
        }

        /// DynamoDB's per-request Limit.
        pub fn limit(mut self, n: u32) -> Self {
            self.r.opts.limit = n as i64;
            self
        }

        /// Makes the reads strongly consistent.
        pub fn consistent(mut self) -> Self {
            self.r.opts.consistent = true;
            self
        }

        /// Resumes after a previous page.
        pub fn cursor(mut self, c: impl Into<Option<Cursor>>) -> Self {
            self.r.cursor = c.into();
            self
        }

        /// Makes `exec` read up to `n` pages (default 1).
        pub fn max_pages(mut self, n: usize) -> Self {
            self.r.pages = n.max(1);
            self
        }

        /// Makes `exec` read every page.
        pub fn all_pages(mut self) -> Self {
            self.r.pages = 0;
            self
        }

        /// Makes `exec` stop after `n` items, returning a cursor to
        /// continue.
        pub fn count(mut self, n: usize) -> Self {
            self.r.count = n;
            self
        }

        /// Returns items without checking that this entity wrote them (by
        /// identifiers).
        pub fn ignore_ownership(mut self) -> Self {
            self.r.opts.ignore_ownership = true;
            self
        }

        /// Overrides the table for this call.
        pub fn table(mut self, name: impl Into<String>) -> Self {
            self.r.o.table = Some(name.into());
            self
        }

        /// Overrides the DynamoDB client for this call.
        pub fn client(mut self, client: impl DynamoDb) -> Self {
            self.r.o.client = Some(Arc::new(client));
            self
        }
    };
}

macro_rules! index_read_methods {
    () => {
        /// Fetches the full items behind a keys-only or partial projection.
        pub fn hydrate(mut self) -> Self {
            self.r.opts.hydrate = true;
            self
        }

        /// Reads only the given attributes, returning partials.
        pub fn attributes(mut self, attrs: impl AttrList<E>) -> PartialRead<E> {
            self.r.opts.attributes = attrs.root_fields();
            PartialRead {
                r: self.r,
                _e: std::marker::PhantomData,
            }
        }

        /// Returns items with their stored key attributes.
        pub fn include_keys(mut self) -> KeyedRead<E> {
            self.r.opts.include_keys = true;
            KeyedRead {
                r: self.r,
                _e: std::marker::PhantomData,
            }
        }

        /// Reads the items, honoring max_pages/all_pages and count, with a
        /// cursor to continue from.
        pub async fn exec(self) -> Result<Page<E>> {
            let (_, res) = self.r.fetch().await?;
            decode_page(&res, &E::decode)
        }

        /// Iterates pages until there are no more.
        pub fn pages(self) -> impl Stream<Item = Result<Page<E>>> + Send {
            self.r
                .pages()
                .map(|r| r.and_then(|(_, res)| decode_page(&res, &E::decode)))
        }

        /// Iterates items until there are no more.
        pub fn all(self) -> impl Stream<Item = Result<E>> + Send {
            items_of(self.pages())
        }
    };
}

/// Reads the items of one access pattern (decisions D29–D31). Generated
/// clients start queries; their sort-key steps narrow them.
pub struct Query<E: Entity> {
    pub(crate) r: Reader,
    _e: std::marker::PhantomData<fn() -> E>,
}

impl<E: Entity> Query<E> {
    pub(crate) fn new(t: &Table<E>, pattern: &str, facets: Vec<(String, Encoded)>) -> Self {
        Query {
            r: Reader::new(t, Kind::Query(pattern.to_string()), facets),
            _e: std::marker::PhantomData,
        }
    }

    read_methods!();
    index_read_methods!();

    /// Fixes a sort-key composite; generated sort-key steps call it.
    #[doc(hidden)]
    pub fn key_eq(mut self, field: &str, v: Result<Value>) -> Self {
        self.r.facets.push(facet(field, v));
        self
    }

    /// Ends the key condition with gt, gte, lt or lte; generated code calls
    /// it.
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

    /// Ends the key condition with an inclusive range.
    #[doc(hidden)]
    pub fn key_between(mut self, field: &str, lo: Result<Value>, hi: Result<Value>) -> Self {
        self.r.sort_key = Some(SortKey::Between(
            field.to_string(),
            facet(field, lo).1,
            facet(field, hi).1,
        ));
        self
    }

    /// Ends the key condition with begins_with.
    #[doc(hidden)]
    pub fn key_begins(mut self, field: &str, v: Result<Value>) -> Self {
        self.r.sort_key = Some(SortKey::Begins(field.to_string(), facet(field, v).1));
        self
    }

    /// Reads the index in descending sort-key order.
    pub fn desc(mut self) -> Self {
        self.r.opts.order = "desc".into();
        self
    }

    /// Reads in ascending order (DynamoDB's default, sent explicitly).
    pub fn asc(mut self) -> Self {
        self.r.opts.order = "asc".into();
        self
    }

    /// How sort-key comparisons treat composites (decision D51).
    pub fn compare(mut self, mode: CompareMode) -> Self {
        self.r.opts.compare = mode.option().into();
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

    /// Returns DynamoDB's responses as they are, honoring max_pages and
    /// all_pages.
    pub async fn raw(self) -> Result<Vec<QueryOutput>> {
        Ok(self.r.raw().await?.0)
    }
}

/// Reads every item of the entity in the table, or in one index.
pub struct Scan<E: Entity> {
    r: Reader,
    _e: std::marker::PhantomData<fn() -> E>,
}

impl<E: Entity> Scan<E> {
    pub(crate) fn new(t: &Table<E>, pattern: &str) -> Self {
        Scan {
            r: Reader::new(t, Kind::Scan(pattern.to_string()), Vec::new()),
            _e: std::marker::PhantomData,
        }
    }

    read_methods!();
    index_read_methods!();

    /// Edits each request before it's sent.
    pub fn mutate(mut self, f: impl Fn(&mut ScanInput) + Send + Sync + 'static) -> Self {
        self.r.mutate_s.push(Arc::new(f));
        self
    }

    /// The first request, without sending it.
    pub fn params(&self) -> Result<ScanInput> {
        let req = self.r.build(false)?;
        self.r.scan_input(&req.params)
    }

    /// Returns DynamoDB's responses as they are.
    pub async fn raw(self) -> Result<Vec<ScanOutput>> {
        Ok(self.r.raw().await?.1)
    }
}

/// The first request of a find: a query, or a scan when no access pattern
/// fits.
#[derive(Clone, Debug)]
pub enum FindParams {
    Query(QueryInput),
    Scan(ScanInput),
}

/// Reads the items matching some attributes, through the best-fitting
/// access pattern or a scan when none fits (decision D44).
pub struct Find<E: Entity> {
    r: Reader,
    _e: std::marker::PhantomData<fn() -> E>,
}

impl<E: Entity> Find<E> {
    pub(crate) fn new(t: &Table<E>, filter: &E::Partial, matching: bool) -> Self {
        let facets = match E::encode_partial(filter) {
            Ok(attrs) => E::fields()
                .iter()
                .filter_map(|f| attrs.get(*f).map(|v| (f.to_string(), Ok(v.clone()))))
                .collect(),
            Err(e) => vec![(String::new(), Err(e.to_string()))],
        };
        let kind = if matching { Kind::Match } else { Kind::Find };
        Find {
            r: Reader::new(t, kind, facets),
            _e: std::marker::PhantomData,
        }
    }

    read_methods!();

    /// Edits each request when the find runs as a query.
    pub fn mutate_query(mut self, f: impl Fn(&mut QueryInput) + Send + Sync + 'static) -> Self {
        self.r.mutate_q.push(Arc::new(f));
        self
    }

    /// Edits each request when the find runs as a scan.
    pub fn mutate_scan(mut self, f: impl Fn(&mut ScanInput) + Send + Sync + 'static) -> Self {
        self.r.mutate_s.push(Arc::new(f));
        self
    }

    /// The first request, without sending it.
    pub fn params(&self) -> Result<FindParams> {
        let req = self.r.build(false)?;
        if req.target == Operation::Scan {
            Ok(FindParams::Scan(self.r.scan_input(&req.params)?))
        } else {
            Ok(FindParams::Query(self.r.query_input(&req.params)?))
        }
    }

    /// Runs the find, reading max_pages pages (1 by default).
    pub async fn exec(self) -> Result<Page<E>> {
        let (_, res) = self.r.fetch().await?;
        decode_page(&res, &E::decode)
    }

    /// Iterates pages until there are no more.
    pub fn pages(self) -> impl Stream<Item = Result<Page<E>>> + Send {
        self.r
            .pages()
            .map(|r| r.and_then(|(_, res)| decode_page(&res, &E::decode)))
    }

    /// Iterates items until there are no more.
    pub fn all(self) -> impl Stream<Item = Result<E>> + Send {
        items_of(self.pages())
    }
}

/// A query or scan returning only some attributes.
pub struct PartialRead<E: Entity> {
    r: Reader,
    _e: std::marker::PhantomData<fn() -> E>,
}

impl<E: Entity> PartialRead<E> {
    pub async fn exec(self) -> Result<Page<E::Partial>> {
        let (_, res) = self.r.fetch().await?;
        decode_page(&res, &E::decode_partial)
    }

    pub fn pages(self) -> impl Stream<Item = Result<Page<E::Partial>>> + Send {
        self.r
            .pages()
            .map(|r| r.and_then(|(_, res)| decode_page(&res, &E::decode_partial)))
    }

    pub fn all(self) -> impl Stream<Item = Result<E::Partial>> + Send {
        items_of(self.pages())
    }
}

/// A query or scan returning items with their keys.
pub struct KeyedRead<E: Entity> {
    r: Reader,
    _e: std::marker::PhantomData<fn() -> E>,
}

impl<E: Entity> KeyedRead<E> {
    pub async fn exec(self) -> Result<Page<WithKeys<E>>> {
        let model = self.r.model;
        let (_, res) = self.r.fetch().await?;
        decode_page(&res, &|m| split_keys::<E>(model, m))
    }

    pub fn pages(self) -> impl Stream<Item = Result<Page<WithKeys<E>>>> + Send {
        let model = self.r.model;
        self.r
            .pages()
            .map(move |r| r.and_then(|(_, res)| decode_page(&res, &|m| split_keys::<E>(model, m))))
    }

    pub fn all(self) -> impl Stream<Item = Result<WithKeys<E>>> + Send {
        items_of(self.pages())
    }
}

impl<E: Entity> Table<E> {
    /// Starts a query on an access pattern; generated clients call it with
    /// the encoded partition-key composites.
    #[doc(hidden)]
    pub fn query(&self, pattern: &str, facets: Vec<(String, Encoded)>) -> Query<E> {
        Query::new(self, pattern, facets)
    }

    /// Reads every item of the entity in the table.
    pub fn scan(&self) -> Scan<E> {
        Scan::new(self, "")
    }

    /// Reads every item of the entity in an access pattern's index;
    /// generated clients call it.
    #[doc(hidden)]
    pub fn scan_index(&self, pattern: &str) -> Scan<E> {
        Scan::new(self, pattern)
    }

    /// Reads the items with the given attributes through the access pattern
    /// that fits best, or a scan when none does.
    pub fn find(&self, filter: E::Partial) -> Find<E> {
        Find::new(self, &filter, false)
    }

    /// `find`, also filtering on every given attribute (ElectroDB's
    /// `match`).
    pub fn matching(&self, filter: E::Partial) -> Find<E> {
        Find::new(self, &filter, true)
    }
}
