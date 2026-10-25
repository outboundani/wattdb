//! Batch gets, puts and deletes, chunked as ElectroDB chunks them.

use std::collections::HashMap;

use indexmap::IndexMap;

use crate::error::Result;
use crate::item::{FormatOptions, format_item, owns_item};
use crate::model::Model;
use crate::pager::{batch_get_keys, item_includes_keys, keys_to_item, owns_keys};
use crate::query::projection;
use crate::request::{BatchGetParams, BatchWriteParams, WriteRequest};
use crate::value::{Attrs, Value, key_string};
use crate::write::{Options, Write};

/// ElectroDB's MaxBatchItems for gets.
pub const MAX_BATCH_GET: usize = 100;
/// ElectroDB's MaxBatchItems for writes.
pub const MAX_BATCH_WRITE: usize = 25;

/// Shapes a batch get.
#[derive(Clone, Debug, Default)]
pub struct BatchGetOptions {
    /// Overrides the table.
    pub table: String,
    pub attributes: Vec<String>,
    pub ignore_ownership: bool,
    pub preserve_order: bool,
    /// True when the request is sent rather than rendered.
    pub exec: bool,
}

/// Mirrors ElectroDB's batch get: requests in chunks of 100, and response
/// formatting with optional order preservation.
pub struct BatchGet<'m> {
    m: &'m Model,
    opt: BatchGetOptions,
    table: String,
    /// The BatchGetItem requests, one per chunk.
    pub requests: Vec<BatchGetParams>,
    slots: Option<HashMap<String, usize>>,
    data: Vec<Option<Attrs>>,
    /// The unprocessed keys, read back as composites.
    pub unprocessed: Vec<Option<Attrs>>,
    /// The same keys as DynamoDB returned them.
    pub unprocessed_keys: Vec<Attrs>,
}

impl<'m> BatchGet<'m> {
    /// Builds the requests for a batch get of table-key composites.
    pub fn new(
        m: &'m Model,
        table: &str,
        composites: &[Attrs],
        opt: BatchGetOptions,
    ) -> Result<Self> {
        let table = if opt.table.is_empty() {
            table.to_string()
        } else {
            opt.table.clone()
        };
        let keys = batch_get_keys(m, composites)?;
        let (mut proj, mut names) = (None, None);
        if !opt.attributes.is_empty() {
            let mut n = IndexMap::new();
            proj = Some(projection(
                m,
                &mut n,
                &opt.attributes,
                opt.exec && !opt.ignore_ownership,
            ));
            names = Some(n);
        }
        let requests = keys
            .chunks(MAX_BATCH_GET)
            .map(|c| BatchGetParams {
                table: table.clone(),
                keys: c.to_vec(),
                projection_expression: proj.clone(),
                expression_attribute_names: names.clone(),
            })
            .collect();
        let mut b = BatchGet {
            m,
            opt,
            table,
            requests,
            slots: None,
            data: Vec::new(),
            unprocessed: Vec::new(),
            unprocessed_keys: Vec::new(),
        };
        if b.opt.preserve_order {
            let mut slots = HashMap::new();
            for k in &keys {
                let ks = b.key_string(k);
                let n = slots.len();
                slots.entry(ks).or_insert(n);
            }
            b.data = vec![None; slots.len()];
            b.slots = Some(slots);
        }
        Ok(b)
    }

    /// The request for unprocessed keys.
    pub fn retry(&self, keys: Vec<Attrs>) -> BatchGetParams {
        let first = self.requests.first();
        BatchGetParams {
            table: self.table.clone(),
            keys,
            projection_expression: first.and_then(|r| r.projection_expression.clone()),
            expression_attribute_names: first.and_then(|r| r.expression_attribute_names.clone()),
        }
    }

    /// The table the requests target.
    pub fn table(&self) -> &str {
        &self.table
    }

    fn key_string(&self, item: &Attrs) -> String {
        let t = self.m.table_index();
        let mut s = key_string(item.get(&t.pk.field).unwrap_or(&Value::Null));
        if t.has_sk {
            s.push_str(&key_string(item.get(&t.sk.field).unwrap_or(&Value::Null)));
        }
        s
    }

    /// Mirrors `applyBulkGetResponseFormatting` for one response: the items
    /// DynamoDB returned and its unprocessed keys.
    pub fn apply(&mut self, items: &[Attrs], unprocessed: &[Attrs]) -> Result<()> {
        let table = self.m.table_index();
        for k in unprocessed {
            self.unprocessed_keys.push(k.clone());
            self.unprocessed.push(keys_to_item(self.m, table, k));
        }
        for item in items {
            let data = if self.owns(item) {
                Some(format_item(
                    self.m,
                    item,
                    &FormatOptions {
                        include_keys: false,
                        attributes: self.opt.attributes.clone(),
                    },
                )?)
            } else {
                None
            };
            let slot = self
                .slots
                .as_ref()
                .and_then(|s| s.get(&self.key_string(item)).copied());
            match slot {
                Some(i) => self.data[i] = data,
                None => self.data.push(data),
            }
        }
        Ok(())
    }

    /// Mirrors `Entity.is` for batch gets.
    fn owns(&self, item: &Attrs) -> bool {
        if self.opt.ignore_ownership
            && !self.opt.attributes.is_empty()
            && !item_includes_keys(self.m, item)
        {
            return true;
        }
        if self.opt.ignore_ownership && owns_keys(self.m, item) {
            return true;
        }
        owns_item(self.m, item)
    }

    /// The formatted items: in key order with `None` for missing ones when
    /// preserving order, otherwise in response order (`None` for items the
    /// entity doesn't own).
    pub fn data(&self) -> &[Option<Attrs>] {
        &self.data
    }
}

/// Shapes a batch write.
#[derive(Clone, Debug, Default)]
pub struct BatchWriteOptions {
    /// Overrides the table.
    pub table: String,
}

/// Mirrors ElectroDB's batch put and delete: requests in chunks of 25 and
/// unprocessed formatting.
pub struct BatchWrite<'m> {
    m: &'m Model,
    table: String,
    /// The BatchWriteItem requests, one per chunk.
    pub requests: Vec<BatchWriteParams>,
    /// The unprocessed writes, formatted: put items as read items, delete
    /// keys as composites.
    pub unprocessed: Vec<Option<Attrs>>,
    /// The same writes as DynamoDB returned them.
    pub unprocessed_requests: Vec<WriteRequest>,
}

impl<'m> BatchWrite<'m> {
    fn new(m: &'m Model, table: &str, opt: &BatchWriteOptions) -> Self {
        let table = if opt.table.is_empty() {
            table.to_string()
        } else {
            opt.table.clone()
        };
        BatchWrite {
            m,
            table,
            requests: Vec::new(),
            unprocessed: Vec::new(),
            unprocessed_requests: Vec::new(),
        }
    }

    /// Builds a batch put of items.
    pub fn put(m: &'m Model, table: &str, items: &[Attrs], opt: BatchWriteOptions) -> Result<Self> {
        let mut b = BatchWrite::new(m, table, &opt);
        let mut reqs = Vec::with_capacity(items.len());
        for it in items {
            let r = Write::put(m, it, false).build(&b.table, Options::default())?;
            reqs.push(WriteRequest::Put(r.params.item.unwrap_or_default()));
        }
        b.chunk(reqs);
        Ok(b)
    }

    /// Builds a batch delete of table-key composites.
    pub fn delete(
        m: &'m Model,
        table: &str,
        keys: &[Attrs],
        opt: BatchWriteOptions,
    ) -> Result<Self> {
        let mut b = BatchWrite::new(m, table, &opt);
        let mut reqs = Vec::with_capacity(keys.len());
        for k in keys {
            let r = Write::delete(m, k, false).build(&b.table, Options::default())?;
            reqs.push(WriteRequest::Delete(r.params.key.unwrap_or_default()));
        }
        b.chunk(reqs);
        Ok(b)
    }

    fn chunk(&mut self, reqs: Vec<WriteRequest>) {
        for c in reqs.chunks(MAX_BATCH_WRITE) {
            self.requests.push(self.retry(c.to_vec()));
        }
    }

    /// The request for unprocessed writes.
    pub fn retry(&self, writes: Vec<WriteRequest>) -> BatchWriteParams {
        BatchWriteParams {
            table: self.table.clone(),
            writes,
        }
    }

    /// The table the requests target.
    pub fn table(&self) -> &str {
        &self.table
    }

    /// Mirrors `formatBulkWriteResponse` for one response's unprocessed
    /// writes.
    pub fn apply(&mut self, unprocessed: &[WriteRequest]) -> Result<()> {
        for req in unprocessed {
            self.unprocessed_requests.push(req.clone());
            match req {
                WriteRequest::Put(item) => {
                    let data = if owns_item(self.m, item) {
                        Some(format_item(self.m, item, &FormatOptions::default())?)
                    } else {
                        None
                    };
                    self.unprocessed.push(data);
                }
                WriteRequest::Delete(key) => {
                    self.unprocessed
                        .push(keys_to_item(self.m, self.m.table_index(), key));
                }
            }
        }
        Ok(())
    }
}
