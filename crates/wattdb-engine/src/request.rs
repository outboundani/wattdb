//! Built DynamoDB requests, in engine values.

use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};

use crate::value::Attrs;
use crate::wire;

/// A DynamoDB operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operation {
    GetItem,
    PutItem,
    DeleteItem,
    UpdateItem,
    /// A transaction's condition check.
    ConditionCheck,
    Query,
    Scan,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::GetItem => "GetItem",
            Operation::PutItem => "PutItem",
            Operation::DeleteItem => "DeleteItem",
            Operation::UpdateItem => "UpdateItem",
            Operation::ConditionCheck => "ConditionCheck",
            Operation::Query => "Query",
            Operation::Scan => "Scan",
        }
    }

    /// The operation's name inside a transaction item.
    pub fn transaction_name(self) -> &'static str {
        match self {
            Operation::GetItem => "Get",
            Operation::PutItem => "Put",
            Operation::UpdateItem => "Update",
            Operation::DeleteItem => "Delete",
            Operation::ConditionCheck => "ConditionCheck",
            Operation::Query => "Query",
            Operation::Scan => "Scan",
        }
    }
}

/// The fields of a single-item, query or scan request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    pub table_name: Option<String>,
    pub index_name: Option<String>,
    pub key: Option<Attrs>,
    pub item: Option<Attrs>,
    pub update_expression: Option<String>,
    pub condition_expression: Option<String>,
    pub key_condition_expression: Option<String>,
    pub filter_expression: Option<String>,
    pub projection_expression: Option<String>,
    pub expression_attribute_names: Option<IndexMap<String, String>>,
    pub expression_attribute_values: Option<Attrs>,
    pub return_values: Option<String>,
    pub return_values_on_condition_check_failure: Option<String>,
    pub consistent_read: Option<bool>,
    pub limit: Option<i64>,
    pub scan_index_forward: Option<bool>,
    pub exclusive_start_key: Option<Attrs>,
}

impl Params {
    /// The request as DynamoDB's JSON protocol body.
    pub fn to_json(&self) -> Json {
        let mut out = Map::new();
        let s = |v: &Option<String>| v.as_ref().map(|s| Json::String(s.clone()));
        let mut put = |k: &str, v: Option<Json>| {
            if let Some(v) = v {
                out.insert(k.to_string(), v);
            }
        };
        put("TableName", s(&self.table_name));
        put("IndexName", s(&self.index_name));
        put("Key", self.key.as_ref().map(wire::attrs_to_json));
        put("Item", self.item.as_ref().map(wire::attrs_to_json));
        put("UpdateExpression", s(&self.update_expression));
        put("ConditionExpression", s(&self.condition_expression));
        put("KeyConditionExpression", s(&self.key_condition_expression));
        put("FilterExpression", s(&self.filter_expression));
        put("ProjectionExpression", s(&self.projection_expression));
        put(
            "ExpressionAttributeNames",
            self.expression_attribute_names.as_ref().map(|n| {
                Json::Object(
                    n.iter()
                        .map(|(k, v)| (k.clone(), Json::String(v.clone())))
                        .collect(),
                )
            }),
        );
        put(
            "ExpressionAttributeValues",
            self.expression_attribute_values
                .as_ref()
                .map(wire::attrs_to_json),
        );
        put("ReturnValues", s(&self.return_values));
        put(
            "ReturnValuesOnConditionCheckFailure",
            s(&self.return_values_on_condition_check_failure),
        );
        put("ConsistentRead", self.consistent_read.map(Json::Bool));
        put("Limit", self.limit.map(|l| json!(l)));
        put("ScanIndexForward", self.scan_index_forward.map(Json::Bool));
        put(
            "ExclusiveStartKey",
            self.exclusive_start_key.as_ref().map(wire::attrs_to_json),
        );
        Json::Object(out)
    }
}

/// A BatchGetItem request for one table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BatchGetParams {
    pub table: String,
    pub keys: Vec<Attrs>,
    pub projection_expression: Option<String>,
    pub expression_attribute_names: Option<IndexMap<String, String>>,
}

impl BatchGetParams {
    pub fn to_json(&self) -> Json {
        let mut body = Map::new();
        body.insert(
            "Keys".into(),
            Json::Array(self.keys.iter().map(wire::attrs_to_json).collect()),
        );
        if let Some(p) = &self.projection_expression {
            body.insert("ProjectionExpression".into(), Json::String(p.clone()));
        }
        if let Some(n) = &self.expression_attribute_names {
            body.insert(
                "ExpressionAttributeNames".into(),
                Json::Object(
                    n.iter()
                        .map(|(k, v)| (k.clone(), Json::String(v.clone())))
                        .collect(),
                ),
            );
        }
        json!({ "RequestItems": { self.table.clone(): Json::Object(body) } })
    }
}

/// One write of a BatchWriteItem request.
#[derive(Clone, Debug, PartialEq)]
pub enum WriteRequest {
    Put(Attrs),
    Delete(Attrs),
}

impl WriteRequest {
    /// The put item or delete key.
    pub fn key(&self) -> &Attrs {
        match self {
            WriteRequest::Put(a) | WriteRequest::Delete(a) => a,
        }
    }

    pub fn to_json(&self) -> Json {
        match self {
            WriteRequest::Put(item) => {
                json!({ "PutRequest": { "Item": wire::attrs_to_json(item) } })
            }
            WriteRequest::Delete(key) => {
                json!({ "DeleteRequest": { "Key": wire::attrs_to_json(key) } })
            }
        }
    }
}

/// A BatchWriteItem request for one table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BatchWriteParams {
    pub table: String,
    pub writes: Vec<WriteRequest>,
}

impl BatchWriteParams {
    pub fn to_json(&self) -> Json {
        json!({ "RequestItems": { self.table.clone(): self.writes.iter().map(WriteRequest::to_json).collect::<Vec<_>>() } })
    }
}

/// A TransactWriteItems or TransactGetItems request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TxParams {
    pub items: Vec<(Operation, Params)>,
    pub token: Option<String>,
}

impl TxParams {
    pub fn to_json(&self) -> Json {
        let items: Vec<Json> = self
            .items
            .iter()
            .map(|(op, p)| {
                let mut m = Map::new();
                m.insert(op.transaction_name().into(), p.to_json());
                Json::Object(m)
            })
            .collect();
        let mut out = Map::new();
        out.insert("TransactItems".into(), Json::Array(items));
        if let Some(t) = &self.token {
            out.insert("ClientRequestToken".into(), Json::String(t.clone()));
        }
        Json::Object(out)
    }
}
