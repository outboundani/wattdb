//! Engine requests in SDK types.

use std::collections::HashMap;

use aws_sdk_dynamodb::operation::batch_get_item::BatchGetItemInput;
use aws_sdk_dynamodb::operation::batch_write_item::BatchWriteItemInput;
use aws_sdk_dynamodb::operation::delete_item::DeleteItemInput;
use aws_sdk_dynamodb::operation::get_item::GetItemInput;
use aws_sdk_dynamodb::operation::put_item::PutItemInput;
use aws_sdk_dynamodb::operation::query::QueryInput;
use aws_sdk_dynamodb::operation::scan::ScanInput;
use aws_sdk_dynamodb::operation::update_item::UpdateItemInput;
use aws_sdk_dynamodb::types::{
    ConditionCheck, Delete, DeleteRequest, Get, KeysAndAttributes, Put, PutRequest, ReturnValue,
    ReturnValuesOnConditionCheckFailure, TransactGetItem, TransactWriteItem, Update, WriteRequest,
};
use indexmap::IndexMap;
use wattdb_engine::request::{
    BatchGetParams, BatchWriteParams, Operation, Params, WriteRequest as EngineWrite,
};

use crate::error::{Error, Result};
use crate::value::{Item, from_item, to_item};

fn build_err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("wattdb: building the request: {e}"))
}

/// A request's fields in SDK types.
pub(crate) struct Sdk {
    table: Option<String>,
    index: Option<String>,
    key: Option<Item>,
    item: Option<Item>,
    update: Option<String>,
    condition: Option<String>,
    key_condition: Option<String>,
    filter: Option<String>,
    projection: Option<String>,
    names: Option<HashMap<String, String>>,
    values: Option<Item>,
    return_values: Option<ReturnValue>,
    return_ccf: Option<ReturnValuesOnConditionCheckFailure>,
    consistent: Option<bool>,
    limit: Option<i32>,
    forward: Option<bool>,
    start_key: Option<Item>,
}

fn names(n: &Option<IndexMap<String, String>>) -> Option<HashMap<String, String>> {
    n.as_ref()
        .filter(|n| !n.is_empty())
        .map(|n| n.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

impl Sdk {
    pub fn from(p: &Params) -> Result<Sdk> {
        let item = |a: &Option<wattdb_engine::Attrs>, what: &str| -> Result<Option<Item>> {
            match a {
                None => Ok(None),
                Some(a) => to_item(a)
                    .map(Some)
                    .map_err(|e| Error::Encode(format!("{what}: {e}"))),
            }
        };
        Ok(Sdk {
            table: p.table_name.clone(),
            index: p.index_name.clone(),
            key: item(&p.key, "Key")?,
            item: item(&p.item, "Item")?,
            update: p.update_expression.clone(),
            condition: p.condition_expression.clone(),
            key_condition: p.key_condition_expression.clone(),
            filter: p.filter_expression.clone(),
            projection: p.projection_expression.clone(),
            names: names(&p.expression_attribute_names),
            values: item(&p.expression_attribute_values, "ExpressionAttributeValues")?
                .filter(|v| !v.is_empty()),
            return_values: p.return_values.as_deref().map(ReturnValue::from),
            return_ccf: p
                .return_values_on_condition_check_failure
                .as_deref()
                .map(ReturnValuesOnConditionCheckFailure::from),
            consistent: p.consistent_read,
            limit: p.limit.map(|l| l.clamp(0, i32::MAX as i64) as i32),
            forward: p.scan_index_forward,
            start_key: item(&p.exclusive_start_key, "ExclusiveStartKey")?,
        })
    }

    pub fn get(self) -> Result<GetItemInput> {
        GetItemInput::builder()
            .set_table_name(self.table)
            .set_key(self.key)
            .set_consistent_read(self.consistent)
            .set_projection_expression(self.projection)
            .set_expression_attribute_names(self.names)
            .build()
            .map_err(build_err)
    }

    pub fn put(self) -> Result<PutItemInput> {
        PutItemInput::builder()
            .set_table_name(self.table)
            .set_item(self.item)
            .set_condition_expression(self.condition)
            .set_expression_attribute_names(self.names)
            .set_expression_attribute_values(self.values)
            .set_return_values(self.return_values)
            .set_return_values_on_condition_check_failure(self.return_ccf)
            .build()
            .map_err(build_err)
    }

    pub fn delete(self) -> Result<DeleteItemInput> {
        DeleteItemInput::builder()
            .set_table_name(self.table)
            .set_key(self.key)
            .set_condition_expression(self.condition)
            .set_expression_attribute_names(self.names)
            .set_expression_attribute_values(self.values)
            .set_return_values(self.return_values)
            .set_return_values_on_condition_check_failure(self.return_ccf)
            .build()
            .map_err(build_err)
    }

    pub fn update(self) -> Result<UpdateItemInput> {
        UpdateItemInput::builder()
            .set_table_name(self.table)
            .set_key(self.key)
            .set_update_expression(self.update)
            .set_condition_expression(self.condition)
            .set_expression_attribute_names(self.names)
            .set_expression_attribute_values(self.values)
            .set_return_values(self.return_values)
            .set_return_values_on_condition_check_failure(self.return_ccf)
            .build()
            .map_err(build_err)
    }

    pub fn query(self) -> Result<QueryInput> {
        QueryInput::builder()
            .set_table_name(self.table)
            .set_index_name(self.index)
            .set_key_condition_expression(self.key_condition)
            .set_filter_expression(self.filter)
            .set_projection_expression(self.projection)
            .set_expression_attribute_names(self.names)
            .set_expression_attribute_values(self.values)
            .set_limit(self.limit)
            .set_scan_index_forward(self.forward)
            .set_consistent_read(self.consistent)
            .set_exclusive_start_key(self.start_key)
            .build()
            .map_err(build_err)
    }

    pub fn scan(self) -> Result<ScanInput> {
        ScanInput::builder()
            .set_table_name(self.table)
            .set_index_name(self.index)
            .set_filter_expression(self.filter)
            .set_projection_expression(self.projection)
            .set_expression_attribute_names(self.names)
            .set_expression_attribute_values(self.values)
            .set_limit(self.limit)
            .set_consistent_read(self.consistent)
            .set_exclusive_start_key(self.start_key)
            .build()
            .map_err(build_err)
    }

    /// A transaction write item; `op` is the engine request's operation.
    pub fn tx_write_item(self, op: Operation) -> Result<TransactWriteItem> {
        let b = TransactWriteItem::builder();
        Ok(match op {
            Operation::PutItem => b
                .put(
                    Put::builder()
                        .set_table_name(self.table)
                        .set_item(self.item)
                        .set_condition_expression(self.condition)
                        .set_expression_attribute_names(self.names)
                        .set_expression_attribute_values(self.values)
                        .set_return_values_on_condition_check_failure(self.return_ccf)
                        .build()
                        .map_err(build_err)?,
                )
                .build(),
            Operation::DeleteItem => b
                .delete(
                    Delete::builder()
                        .set_table_name(self.table)
                        .set_key(self.key)
                        .set_condition_expression(self.condition)
                        .set_expression_attribute_names(self.names)
                        .set_expression_attribute_values(self.values)
                        .set_return_values_on_condition_check_failure(self.return_ccf)
                        .build()
                        .map_err(build_err)?,
                )
                .build(),
            Operation::UpdateItem => b
                .update(
                    Update::builder()
                        .set_table_name(self.table)
                        .set_key(self.key)
                        .set_update_expression(self.update)
                        .set_condition_expression(self.condition)
                        .set_expression_attribute_names(self.names)
                        .set_expression_attribute_values(self.values)
                        .set_return_values_on_condition_check_failure(self.return_ccf)
                        .build()
                        .map_err(build_err)?,
                )
                .build(),
            Operation::ConditionCheck => b
                .condition_check(
                    ConditionCheck::builder()
                        .set_table_name(self.table)
                        .set_key(self.key)
                        .set_condition_expression(self.condition)
                        .set_expression_attribute_names(self.names)
                        .set_expression_attribute_values(self.values)
                        .set_return_values_on_condition_check_failure(self.return_ccf)
                        .build()
                        .map_err(build_err)?,
                )
                .build(),
            op => {
                return Err(Error::Invalid(format!(
                    "wattdb: {} can't be a transaction write",
                    op.as_str()
                )));
            }
        })
    }

    pub fn tx_get_item(self) -> Result<TransactGetItem> {
        Ok(TransactGetItem::builder()
            .get(
                Get::builder()
                    .set_table_name(self.table)
                    .set_key(self.key)
                    .set_projection_expression(self.projection)
                    .set_expression_attribute_names(self.names)
                    .build()
                    .map_err(build_err)?,
            )
            .build())
    }
}

pub(crate) fn batch_get_input(p: &BatchGetParams) -> Result<BatchGetItemInput> {
    let keys: Vec<Item> = p.keys.iter().map(to_item).collect::<Result<_>>()?;
    let ka = KeysAndAttributes::builder()
        .set_keys(Some(keys))
        .set_projection_expression(p.projection_expression.clone())
        .set_expression_attribute_names(names(&p.expression_attribute_names))
        .build()
        .map_err(build_err)?;
    BatchGetItemInput::builder()
        .request_items(p.table.clone(), ka)
        .build()
        .map_err(build_err)
}

pub(crate) fn batch_write_input(p: &BatchWriteParams) -> Result<BatchWriteItemInput> {
    let mut writes = Vec::with_capacity(p.writes.len());
    for w in &p.writes {
        writes.push(match w {
            EngineWrite::Put(item) => WriteRequest::builder()
                .put_request(
                    PutRequest::builder()
                        .set_item(Some(to_item(item)?))
                        .build()
                        .map_err(build_err)?,
                )
                .build(),
            EngineWrite::Delete(key) => WriteRequest::builder()
                .delete_request(
                    DeleteRequest::builder()
                        .set_key(Some(to_item(key)?))
                        .build()
                        .map_err(build_err)?,
                )
                .build(),
        });
    }
    BatchWriteItemInput::builder()
        .request_items(p.table.clone(), writes)
        .build()
        .map_err(build_err)
}

/// A batch write's unprocessed requests in engine form.
pub(crate) fn unprocessed_writes(reqs: &[WriteRequest]) -> Vec<EngineWrite> {
    reqs.iter()
        .filter_map(|w| {
            if let Some(p) = w.put_request() {
                return Some(EngineWrite::Put(from_item(p.item())));
            }
            w.delete_request()
                .map(|d| EngineWrite::Delete(from_item(d.key())))
        })
        .collect()
}
