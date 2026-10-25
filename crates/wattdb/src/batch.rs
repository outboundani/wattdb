//! Batch gets, puts and deletes (decision D41).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_dynamodb::operation::batch_get_item::BatchGetItemInput;
use aws_sdk_dynamodb::operation::batch_write_item::BatchWriteItemInput;
use futures::stream::{self, StreamExt, TryStreamExt};
use wattdb_engine::batch::{
    BatchGet as EngineBatchGet, BatchGetOptions, BatchWrite as EngineBatchWrite, BatchWriteOptions,
};
use wattdb_engine::request::{BatchGetParams, BatchWriteParams, WriteRequest};
use wattdb_engine::value::key_string;

use crate::client::{DynamoDb, Request};
use crate::entity::{Entity, Overrides, Table};
use crate::error::{Error, Result};
use crate::ops::{Encoded, encoded};
use crate::sdk::{batch_get_input, batch_write_input, unprocessed_writes};
use crate::value::{Attrs, Value, from_item};

/// How `retry_unprocessed` retries: how many times, and the delay before
/// each retry.
#[derive(Clone)]
pub struct Backoff {
    /// Retries after the first attempt.
    pub retries: u32,
    delay: Arc<dyn Fn(u32) -> Duration + Send + Sync>,
}

impl Backoff {
    /// Retries up to `retries` times, waiting `base`, 2×`base`, 4×`base`, ...
    pub fn new(retries: u32, base: Duration) -> Self {
        Backoff {
            retries,
            delay: Arc::new(move |n| base.saturating_mul(1 << n.min(31))),
        }
    }

    /// Retries with a custom delay before retry `n` (starting at 0).
    pub fn with_delay(
        retries: u32,
        delay: impl Fn(u32) -> Duration + Send + Sync + 'static,
    ) -> Self {
        Backoff {
            retries,
            delay: Arc::new(delay),
        }
    }

    /// The delay before retry `n`.
    pub fn delay(&self, n: u32) -> Duration {
        (self.delay)(n)
    }
}

impl std::fmt::Debug for Backoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backoff")
            .field("retries", &self.retries)
            .finish()
    }
}

#[derive(Clone, Default)]
struct BatchOptions {
    o: Overrides,
    concurrent: usize,
    retry: Option<Backoff>,
}

/// Maps table keys back to the caller's values, so unprocessed work comes
/// back as what was passed in.
struct KeyIndex {
    by_key: HashMap<String, Vec<usize>>,
    pk: String,
    sk: Option<String>,
}

impl KeyIndex {
    fn new(m: &wattdb_engine::Model) -> Self {
        let t = m.table_index();
        KeyIndex {
            by_key: HashMap::new(),
            pk: t.pk.field.clone(),
            sk: t.has_sk.then(|| t.sk.field.clone()),
        }
    }

    fn id(&self, item: &Attrs) -> String {
        let mut id = key_string(item.get(&self.pk).unwrap_or(&Value::Null));
        if let Some(sk) = &self.sk {
            id.push('\0');
            id.push_str(&key_string(item.get(sk).unwrap_or(&Value::Null)));
        }
        id
    }

    fn add(&mut self, item: &Attrs, i: usize) {
        let id = self.id(item);
        self.by_key.entry(id).or_default().push(i);
    }

    fn lookup(&self, item: &Attrs) -> &[usize] {
        self.by_key
            .get(&self.id(item))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Picks the caller's values for unprocessed keys, each once.
fn unprocessed_inputs<T: Clone>(idx: &KeyIndex, inputs: &[T], keys: &[&Attrs]) -> Vec<T> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for k in keys {
        for &i in idx.lookup(k) {
            if seen.insert(i) {
                out.push(inputs[i].clone());
            }
        }
    }
    out
}

async fn sleep(d: Duration) {
    tokio::time::sleep(d).await
}

/// What a batch get found.
#[derive(Debug)]
pub struct BatchGetOutput<E: Entity> {
    /// The items found, in response order.
    pub found: Vec<E>,
    /// The keys DynamoDB left unprocessed.
    pub unprocessed: Vec<E::Key>,
}

/// What an order-preserving batch get found.
#[derive(Debug)]
pub struct OrderedBatchGetOutput<E: Entity> {
    /// One entry per distinct key, `None` where the item is missing or
    /// unprocessed.
    pub items: Vec<Option<E>>,
    pub unprocessed: Vec<E::Key>,
}

type Mutator<I> = Arc<dyn Fn(&mut I) + Send + Sync>;

/// Reads many items by key, in chunks of 100.
pub struct BatchGet<E: Entity> {
    t: Table<E>,
    keys: Vec<E::Key>,
    encoded: std::result::Result<Vec<Attrs>, String>,
    b: BatchOptions,
    mutate: Vec<Mutator<BatchGetItemInput>>,
}

impl<E: Entity> BatchGet<E> {
    pub(crate) fn new(t: &Table<E>, keys: Vec<E::Key>) -> Self {
        let encoded = keys
            .iter()
            .map(|k| encoded(E::encode_key(k)))
            .collect::<std::result::Result<Vec<_>, _>>();
        BatchGet {
            t: t.clone(),
            keys,
            encoded,
            b: BatchOptions::default(),
            mutate: Vec::new(),
        }
    }

    /// Sends up to `n` chunks at once (ElectroDB's `concurrent`; 1 by
    /// default).
    pub fn concurrent(mut self, n: usize) -> Self {
        self.b.concurrent = n;
        self
    }

    /// Resends unprocessed keys with backoff.
    pub fn retry_unprocessed(mut self, policy: Backoff) -> Self {
        self.b.retry = Some(policy);
        self
    }

    pub fn table(mut self, name: impl Into<String>) -> Self {
        self.b.o.table = Some(name.into());
        self
    }

    pub fn client(mut self, client: impl DynamoDb) -> Self {
        self.b.o.client = Some(Arc::new(client));
        self
    }

    /// Edits each request before it's sent.
    pub fn mutate(mut self, f: impl Fn(&mut BatchGetItemInput) + Send + Sync + 'static) -> Self {
        self.mutate.push(Arc::new(f));
        self
    }

    /// Returns the items aligned with the keys, `None` where missing.
    pub fn preserve_order(self) -> OrderedBatchGet<E> {
        OrderedBatchGet(self)
    }

    fn build(&self, exec: bool, preserve: bool) -> Result<EngineBatchGet<'static>> {
        let composites = self.encoded.clone().map_err(Error::Encode)?;
        let opt = BatchGetOptions {
            table: self.b.o.table.clone().unwrap_or_default(),
            preserve_order: preserve,
            exec,
            ..Default::default()
        };
        Ok(EngineBatchGet::new(
            self.t.model(),
            self.t.name(),
            &composites,
            opt,
        )?)
    }

    fn input(&self, p: &BatchGetParams) -> Result<BatchGetItemInput> {
        let mut input = batch_get_input(p)?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok(input)
    }

    /// The requests, one per chunk, without sending them.
    pub fn params(&self) -> Result<Vec<BatchGetItemInput>> {
        self.build(false, false)?
            .requests
            .iter()
            .map(|p| self.input(p))
            .collect()
    }

    /// Sends one chunk, retrying its unprocessed keys; returns each
    /// attempt's items and the keys left at the end.
    async fn chunk(&self, req: BatchGetParams) -> Result<(Vec<Attrs>, Vec<Attrs>)> {
        let mut req = req;
        let mut found = Vec::new();
        let mut attempt = 0;
        loop {
            let input = self.input(&req)?;
            let out = self
                .t
                .send(&self.b.o, Request::BatchGetItem(input))
                .await?
                .batch_get_item()?;
            if let Some(items) = out.responses.as_ref().and_then(|r| r.get(&req.table)) {
                found.extend(items.iter().map(from_item));
            }
            let unprocessed: Vec<Attrs> = out
                .unprocessed_keys
                .as_ref()
                .and_then(|u| u.get(&req.table))
                .map(|ka| ka.keys().iter().map(from_item).collect())
                .unwrap_or_default();
            match &self.b.retry {
                Some(p) if !unprocessed.is_empty() && attempt < p.retries => {
                    sleep(p.delay(attempt)).await;
                    attempt += 1;
                    req = BatchGetParams {
                        keys: unprocessed,
                        ..req
                    };
                }
                _ => return Ok((found, unprocessed)),
            }
        }
    }

    async fn run(&self, preserve: bool) -> Result<(EngineBatchGet<'static>, Vec<E::Key>)> {
        let mut b = self.build(true, preserve)?;
        let results: Vec<(Vec<Attrs>, Vec<Attrs>)> = stream::iter(b.requests.clone())
            .map(|req| self.chunk(req))
            .buffered(self.b.concurrent.max(1))
            .try_collect()
            .await?;
        for (found, unprocessed) in &results {
            b.apply(found, unprocessed)?;
        }
        let mut idx = KeyIndex::new(self.t.model());
        let mut i = 0;
        for req in &b.requests {
            for k in &req.keys {
                idx.add(k, i);
                i += 1;
            }
        }
        let keys: Vec<&Attrs> = b.unprocessed_keys.iter().collect();
        let unprocessed = unprocessed_inputs(&idx, &self.keys, &keys);
        Ok((b, unprocessed))
    }

    /// Reads the items, returning those found and the keys DynamoDB left
    /// unprocessed.
    pub async fn exec(self) -> Result<BatchGetOutput<E>> {
        let (b, unprocessed) = self.run(false).await?;
        let found = b
            .data()
            .iter()
            .flatten()
            .map(E::decode)
            .collect::<Result<_>>()?;
        Ok(BatchGetOutput { found, unprocessed })
    }
}

/// A batch get whose results line up with its keys.
pub struct OrderedBatchGet<E: Entity>(BatchGet<E>);

impl<E: Entity> OrderedBatchGet<E> {
    /// Reads the items: one entry per distinct key, `None` where the item
    /// is missing or unprocessed.
    pub async fn exec(self) -> Result<OrderedBatchGetOutput<E>> {
        let (b, unprocessed) = self.0.run(true).await?;
        let items = b
            .data()
            .iter()
            .map(|d| d.as_ref().map(E::decode).transpose())
            .collect::<Result<_>>()?;
        Ok(OrderedBatchGetOutput { items, unprocessed })
    }
}

/// Puts or deletes many items, in chunks of 25. `T` is what the caller
/// passed: the entity's `Input` for puts, its `Key` for deletes.
pub struct BatchWrite<E: Entity, T> {
    t: Table<E>,
    put: bool,
    inputs: Vec<T>,
    encoded: std::result::Result<Vec<Attrs>, String>,
    b: BatchOptions,
    mutate: Vec<Mutator<BatchWriteItemInput>>,
}

impl<E: Entity, T: Clone + Send + Sync> BatchWrite<E, T> {
    pub(crate) fn new(
        t: &Table<E>,
        put: bool,
        inputs: Vec<T>,
        encode: impl Fn(&T) -> Result<Attrs>,
    ) -> Self {
        let encoded = inputs
            .iter()
            .map(|i| -> Encoded { encoded(encode(i)) })
            .collect::<std::result::Result<Vec<_>, _>>();
        BatchWrite {
            t: t.clone(),
            put,
            inputs,
            encoded,
            b: BatchOptions::default(),
            mutate: Vec::new(),
        }
    }

    /// Sends up to `n` chunks at once.
    pub fn concurrent(mut self, n: usize) -> Self {
        self.b.concurrent = n;
        self
    }

    /// Resends unprocessed writes with backoff.
    pub fn retry_unprocessed(mut self, policy: Backoff) -> Self {
        self.b.retry = Some(policy);
        self
    }

    pub fn table(mut self, name: impl Into<String>) -> Self {
        self.b.o.table = Some(name.into());
        self
    }

    pub fn client(mut self, client: impl DynamoDb) -> Self {
        self.b.o.client = Some(Arc::new(client));
        self
    }

    /// Edits each request before it's sent.
    pub fn mutate(mut self, f: impl Fn(&mut BatchWriteItemInput) + Send + Sync + 'static) -> Self {
        self.mutate.push(Arc::new(f));
        self
    }

    fn build(&self) -> Result<EngineBatchWrite<'static>> {
        let items = self.encoded.clone().map_err(Error::Encode)?;
        let opt = BatchWriteOptions {
            table: self.b.o.table.clone().unwrap_or_default(),
        };
        let m = self.t.model();
        Ok(if self.put {
            EngineBatchWrite::put(m, self.t.name(), &items, opt)?
        } else {
            EngineBatchWrite::delete(m, self.t.name(), &items, opt)?
        })
    }

    fn input(&self, p: &BatchWriteParams) -> Result<BatchWriteItemInput> {
        let mut input = batch_write_input(p)?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok(input)
    }

    /// The requests, one per chunk, without sending them.
    pub fn params(&self) -> Result<Vec<BatchWriteItemInput>> {
        self.build()?
            .requests
            .iter()
            .map(|p| self.input(p))
            .collect()
    }

    async fn chunk(&self, req: BatchWriteParams) -> Result<Vec<WriteRequest>> {
        let mut req = req;
        let mut attempt = 0;
        loop {
            let input = self.input(&req)?;
            let out = self
                .t
                .send(&self.b.o, Request::BatchWriteItem(input))
                .await?
                .batch_write_item()?;
            let unprocessed = out
                .unprocessed_items
                .as_ref()
                .and_then(|u| u.get(&req.table))
                .map(|w| unprocessed_writes(w))
                .unwrap_or_default();
            match &self.b.retry {
                Some(p) if !unprocessed.is_empty() && attempt < p.retries => {
                    sleep(p.delay(attempt)).await;
                    attempt += 1;
                    req = BatchWriteParams {
                        writes: unprocessed,
                        ..req
                    };
                }
                _ => return Ok(unprocessed),
            }
        }
    }

    /// Sends the writes, returning the inputs DynamoDB left unprocessed.
    pub async fn exec(self) -> Result<Vec<T>> {
        let b = self.build()?;
        let left: Vec<Vec<WriteRequest>> = stream::iter(b.requests.clone())
            .map(|req| self.chunk(req))
            .buffered(self.b.concurrent.max(1))
            .try_collect()
            .await?;
        let mut idx = KeyIndex::new(self.t.model());
        let mut i = 0;
        for req in &b.requests {
            for w in &req.writes {
                idx.add(w.key(), i);
                i += 1;
            }
        }
        let keys: Vec<&Attrs> = left.iter().flatten().map(WriteRequest::key).collect();
        Ok(unprocessed_inputs(&idx, &self.inputs, &keys))
    }
}

impl<E: Entity> Table<E> {
    /// Reads many items by key, in chunks of 100.
    pub fn batch_get(&self, keys: impl IntoIterator<Item = E::Key>) -> BatchGet<E> {
        BatchGet::new(self, keys.into_iter().collect())
    }

    /// Writes many items, in chunks of 25.
    pub fn batch_put(&self, inputs: impl IntoIterator<Item = E::Input>) -> BatchWrite<E, E::Input> {
        BatchWrite::new(self, true, inputs.into_iter().collect(), E::encode_input)
    }

    /// Deletes many items, in chunks of 25.
    pub fn batch_delete(&self, keys: impl IntoIterator<Item = E::Key>) -> BatchWrite<E, E::Key> {
        BatchWrite::new(self, false, keys.into_iter().collect(), E::encode_key)
    }
}

/// The most writes one BatchWriteItem request holds.
pub const MAX_BATCH_WRITE: usize = wattdb_engine::batch::MAX_BATCH_WRITE;
