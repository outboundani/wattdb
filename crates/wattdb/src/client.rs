//! The DynamoDB client, shared configuration and middleware hooks
//! (decisions D25, D50).

use std::future::Future;
use std::sync::Arc;

use aws_sdk_dynamodb::operation::batch_get_item::{BatchGetItemInput, BatchGetItemOutput};
use aws_sdk_dynamodb::operation::batch_write_item::{BatchWriteItemInput, BatchWriteItemOutput};
use aws_sdk_dynamodb::operation::delete_item::{DeleteItemInput, DeleteItemOutput};
use aws_sdk_dynamodb::operation::get_item::{GetItemInput, GetItemOutput};
use aws_sdk_dynamodb::operation::put_item::{PutItemInput, PutItemOutput};
use aws_sdk_dynamodb::operation::query::{QueryInput, QueryOutput};
use aws_sdk_dynamodb::operation::scan::{ScanInput, ScanOutput};
use aws_sdk_dynamodb::operation::transact_get_items::{
    TransactGetItemsInput, TransactGetItemsOutput,
};
use aws_sdk_dynamodb::operation::transact_write_items::{
    TransactWriteItemsInput, TransactWriteItemsOutput,
};
use aws_sdk_dynamodb::operation::update_item::{UpdateItemInput, UpdateItemOutput};
use futures::future::BoxFuture;

use crate::error::{Error, Result};

/// A request wattdb sends, as its SDK input.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Request {
    GetItem(GetItemInput),
    PutItem(PutItemInput),
    DeleteItem(DeleteItemInput),
    UpdateItem(UpdateItemInput),
    Query(QueryInput),
    Scan(ScanInput),
    BatchGetItem(BatchGetItemInput),
    BatchWriteItem(BatchWriteItemInput),
    TransactGetItems(TransactGetItemsInput),
    TransactWriteItems(TransactWriteItemsInput),
}

/// A request's response, as its SDK output.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Response {
    GetItem(GetItemOutput),
    PutItem(PutItemOutput),
    DeleteItem(DeleteItemOutput),
    UpdateItem(UpdateItemOutput),
    Query(QueryOutput),
    Scan(ScanOutput),
    BatchGetItem(BatchGetItemOutput),
    BatchWriteItem(BatchWriteItemOutput),
    TransactGetItems(TransactGetItemsOutput),
    TransactWriteItems(TransactWriteItemsOutput),
}

impl Request {
    /// The DynamoDB operation: `GetItem`, `Query`, `BatchWriteItem`, ...
    pub fn operation(&self) -> &'static str {
        match self {
            Request::GetItem(_) => "GetItem",
            Request::PutItem(_) => "PutItem",
            Request::DeleteItem(_) => "DeleteItem",
            Request::UpdateItem(_) => "UpdateItem",
            Request::Query(_) => "Query",
            Request::Scan(_) => "Scan",
            Request::BatchGetItem(_) => "BatchGetItem",
            Request::BatchWriteItem(_) => "BatchWriteItem",
            Request::TransactGetItems(_) => "TransactGetItems",
            Request::TransactWriteItems(_) => "TransactWriteItems",
        }
    }

    /// The request's table; `None` for batches and transactions, which name
    /// their tables per item.
    pub fn table_name(&self) -> Option<&str> {
        match self {
            Request::GetItem(i) => i.table_name(),
            Request::PutItem(i) => i.table_name(),
            Request::DeleteItem(i) => i.table_name(),
            Request::UpdateItem(i) => i.table_name(),
            Request::Query(i) => i.table_name(),
            Request::Scan(i) => i.table_name(),
            _ => None,
        }
    }
}

macro_rules! response_into {
    ($($variant:ident => $out:ty, $fn:ident;)*) => {
        impl Response {
            $(
                pub(crate) fn $fn(self) -> Result<$out> {
                    match self {
                        Response::$variant(o) => Ok(o),
                        _ => Err(Error::Invalid(concat!("wattdb: expected a ", stringify!($variant), " response").into())),
                    }
                }
            )*
        }
    };
}

response_into! {
    GetItem => GetItemOutput, get_item;
    PutItem => PutItemOutput, put_item;
    DeleteItem => DeleteItemOutput, delete_item;
    UpdateItem => UpdateItemOutput, update_item;
    Query => QueryOutput, query;
    Scan => ScanOutput, scan;
    BatchGetItem => BatchGetItemOutput, batch_get_item;
    BatchWriteItem => BatchWriteItemOutput, batch_write_item;
    TransactGetItems => TransactGetItemsOutput, transact_get_items;
    TransactWriteItems => TransactWriteItemsOutput, transact_write_items;
}

/// What wattdb needs from a DynamoDB client. `aws_sdk_dynamodb::Client`
/// implements it; tests can implement it with a fake.
pub trait DynamoDb: Send + Sync + 'static {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response>>;
}

impl<T: DynamoDb> DynamoDb for Arc<T> {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response>> {
        (**self).send(request)
    }
}

impl DynamoDb for aws_sdk_dynamodb::Client {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response>> {
        Box::pin(async move {
            let r = match request {
                Request::GetItem(i) => Response::GetItem(
                    self.get_item()
                        .set_table_name(i.table_name)
                        .set_key(i.key)
                        .set_attributes_to_get(i.attributes_to_get)
                        .set_consistent_read(i.consistent_read)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .set_projection_expression(i.projection_expression)
                        .set_expression_attribute_names(i.expression_attribute_names)
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::PutItem(i) => Response::PutItem(
                    self.put_item()
                        .set_table_name(i.table_name)
                        .set_item(i.item)
                        .set_expected(i.expected)
                        .set_return_values(i.return_values)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .set_return_item_collection_metrics(i.return_item_collection_metrics)
                        .set_conditional_operator(i.conditional_operator)
                        .set_condition_expression(i.condition_expression)
                        .set_expression_attribute_names(i.expression_attribute_names)
                        .set_expression_attribute_values(i.expression_attribute_values)
                        .set_return_values_on_condition_check_failure(
                            i.return_values_on_condition_check_failure,
                        )
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::DeleteItem(i) => Response::DeleteItem(
                    self.delete_item()
                        .set_table_name(i.table_name)
                        .set_key(i.key)
                        .set_expected(i.expected)
                        .set_conditional_operator(i.conditional_operator)
                        .set_return_values(i.return_values)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .set_return_item_collection_metrics(i.return_item_collection_metrics)
                        .set_condition_expression(i.condition_expression)
                        .set_expression_attribute_names(i.expression_attribute_names)
                        .set_expression_attribute_values(i.expression_attribute_values)
                        .set_return_values_on_condition_check_failure(
                            i.return_values_on_condition_check_failure,
                        )
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::UpdateItem(i) => Response::UpdateItem(
                    self.update_item()
                        .set_table_name(i.table_name)
                        .set_key(i.key)
                        .set_attribute_updates(i.attribute_updates)
                        .set_expected(i.expected)
                        .set_conditional_operator(i.conditional_operator)
                        .set_return_values(i.return_values)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .set_return_item_collection_metrics(i.return_item_collection_metrics)
                        .set_update_expression(i.update_expression)
                        .set_condition_expression(i.condition_expression)
                        .set_expression_attribute_names(i.expression_attribute_names)
                        .set_expression_attribute_values(i.expression_attribute_values)
                        .set_return_values_on_condition_check_failure(
                            i.return_values_on_condition_check_failure,
                        )
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::Query(i) => Response::Query(
                    self.query()
                        .set_table_name(i.table_name)
                        .set_index_name(i.index_name)
                        .set_select(i.select)
                        .set_attributes_to_get(i.attributes_to_get)
                        .set_limit(i.limit)
                        .set_consistent_read(i.consistent_read)
                        .set_key_conditions(i.key_conditions)
                        .set_query_filter(i.query_filter)
                        .set_conditional_operator(i.conditional_operator)
                        .set_scan_index_forward(i.scan_index_forward)
                        .set_exclusive_start_key(i.exclusive_start_key)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .set_projection_expression(i.projection_expression)
                        .set_filter_expression(i.filter_expression)
                        .set_key_condition_expression(i.key_condition_expression)
                        .set_expression_attribute_names(i.expression_attribute_names)
                        .set_expression_attribute_values(i.expression_attribute_values)
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::Scan(i) => Response::Scan(
                    self.scan()
                        .set_table_name(i.table_name)
                        .set_index_name(i.index_name)
                        .set_attributes_to_get(i.attributes_to_get)
                        .set_limit(i.limit)
                        .set_select(i.select)
                        .set_scan_filter(i.scan_filter)
                        .set_conditional_operator(i.conditional_operator)
                        .set_exclusive_start_key(i.exclusive_start_key)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .set_total_segments(i.total_segments)
                        .set_segment(i.segment)
                        .set_projection_expression(i.projection_expression)
                        .set_filter_expression(i.filter_expression)
                        .set_expression_attribute_names(i.expression_attribute_names)
                        .set_expression_attribute_values(i.expression_attribute_values)
                        .set_consistent_read(i.consistent_read)
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::BatchGetItem(i) => Response::BatchGetItem(
                    self.batch_get_item()
                        .set_request_items(i.request_items)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::BatchWriteItem(i) => Response::BatchWriteItem(
                    self.batch_write_item()
                        .set_request_items(i.request_items)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .set_return_item_collection_metrics(i.return_item_collection_metrics)
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::TransactGetItems(i) => Response::TransactGetItems(
                    self.transact_get_items()
                        .set_transact_items(i.transact_items)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
                Request::TransactWriteItems(i) => Response::TransactWriteItems(
                    self.transact_write_items()
                        .set_transact_items(i.transact_items)
                        .set_return_consumed_capacity(i.return_consumed_capacity)
                        .set_return_item_collection_metrics(i.return_item_collection_metrics)
                        .set_client_request_token(i.client_request_token)
                        .send()
                        .await
                        .map_err(sdk)?,
                ),
            };
            Ok(r)
        })
    }
}

fn sdk<E>(e: E) -> Error
where
    aws_sdk_dynamodb::Error: From<E>,
{
    Error::from_sdk(aws_sdk_dynamodb::Error::from(e))
}

/// One request as a hook sees it (decision D50).
#[derive(Clone, Debug)]
pub struct Call {
    /// The entity making the request; `None` for transactions and
    /// collection queries, which can span entities.
    pub entity: Option<String>,
    /// The request; hooks may change it before calling `next`.
    pub request: Request,
}

impl Call {
    /// The DynamoDB operation.
    pub fn operation(&self) -> &'static str {
        self.request.operation()
    }

    /// The request's table; `None` for batches and transactions.
    pub fn table(&self) -> Option<&str> {
        self.request.table_name()
    }
}

/// The rest of the hook chain, ending with the client.
pub struct Next {
    hooks: Arc<[Arc<dyn Hook>]>,
    index: usize,
    client: Arc<dyn DynamoDb>,
}

impl Next {
    /// Runs the remaining hooks and sends the request.
    pub fn run(self, call: Call) -> BoxFuture<'static, Result<Response>> {
        Box::pin(async move {
            if self.index == self.hooks.len() {
                return self.client.send(call.request).await;
            }
            let hook = self.hooks[self.index].clone();
            let next = Next {
                hooks: self.hooks.clone(),
                index: self.index + 1,
                client: self.client.clone(),
            };
            hook.call(call, next).await
        })
    }
}

/// Runs around every request: it may inspect or change the call, run `next`
/// to send it (or not), and inspect or replace the response. A hook that
/// answers without running `next` must return the operation's own
/// response variant.
///
/// Closures are hooks:
///
/// ```
/// # use wattdb::{Call, Config, Next};
/// # fn f(cfg: Config) -> Config {
/// cfg.with_hook(|call: Call, next: Next| async move {
///     println!("{}", call.operation());
///     next.run(call).await
/// })
/// # }
/// ```
pub trait Hook: Send + Sync + 'static {
    fn call(&self, call: Call, next: Next) -> BoxFuture<'static, Result<Response>>;
}

impl<F, Fut> Hook for F
where
    F: Fn(Call, Next) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Response>> + Send + 'static,
{
    fn call(&self, call: Call, next: Next) -> BoxFuture<'static, Result<Response>> {
        Box::pin(self(call, next))
    }
}

/// Logs each request with `tracing`: successful ones at debug level, failed
/// ones at error level, with the operation, entity, table and duration.
#[cfg(feature = "tracing")]
pub struct TracingHook;

#[cfg(feature = "tracing")]
impl Hook for TracingHook {
    fn call(&self, call: Call, next: Next) -> BoxFuture<'static, Result<Response>> {
        Box::pin(async move {
            let operation = call.operation();
            let entity = call.entity.clone().unwrap_or_default();
            let table = call.table().unwrap_or_default().to_string();
            let start = std::time::Instant::now();
            let out = next.run(call).await;
            let duration = start.elapsed();
            match &out {
                Ok(_) => tracing::debug!(operation, entity, table, ?duration, "wattdb request"),
                Err(error) => {
                    tracing::error!(operation, entity, table, ?duration, %error, "wattdb request failed")
                }
            }
            out
        })
    }
}

/// Configuration shared by every client and by transactions (decision D25):
/// the DynamoDB client and the hooks. It's cheap to clone.
#[derive(Clone)]
pub struct Config {
    client: Arc<dyn DynamoDb>,
    hooks: Arc<[Arc<dyn Hook>]>,
}

impl Config {
    /// A configuration sending every request through `client`.
    pub fn new(client: impl DynamoDb) -> Self {
        Config {
            client: Arc::new(client),
            hooks: Arc::new([]),
        }
    }

    /// Adds a hook; the first one added is the outermost.
    pub fn with_hook(self, hook: impl Hook) -> Self {
        let mut hooks: Vec<Arc<dyn Hook>> = self.hooks.iter().cloned().collect();
        hooks.push(Arc::new(hook));
        Config {
            client: self.client,
            hooks: hooks.into(),
        }
    }

    /// Logs every request with `tracing` (see [`TracingHook`]). It runs in
    /// the order added, like any hook.
    #[cfg(feature = "tracing")]
    pub fn with_tracing(self) -> Self {
        self.with_hook(TracingHook)
    }

    /// Sends a request through the hooks, using `client` instead of the
    /// configured client when given.
    pub async fn send(
        &self,
        entity: Option<&str>,
        client: Option<&Arc<dyn DynamoDb>>,
        request: Request,
    ) -> Result<Response> {
        let next = Next {
            hooks: self.hooks.clone(),
            index: 0,
            client: client.cloned().unwrap_or_else(|| self.client.clone()),
        };
        next.run(Call {
            entity: entity.map(String::from),
            request,
        })
        .await
    }
}
