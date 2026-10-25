//! Transactions (decision D42).

use std::any::Any;
use std::marker::PhantomData;
use std::sync::Arc;

use aws_sdk_dynamodb::operation::transact_get_items::TransactGetItemsInput;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsInput;
use wattdb_engine::write::{Request as EngineRequest, Write};

use crate::attr::Cond;
use crate::client::{Config, DynamoDb, Request};
use crate::entity::{Entity, Table};
use crate::error::{Error, Result, SdkError, TxCanceledError, TxReason};
use crate::ops::{Base, Encoded, Get, encoded, take};
use crate::sdk::Sdk;
use crate::value::{Attrs, from_item};

type Build = Box<dyn Fn(bool) -> Result<EngineRequest<'static>> + Send + Sync>;
type DecodeAny = fn(&Attrs) -> Result<Box<dyn Any + Send + Sync>>;
type Mutator<I> = Arc<dyn Fn(&mut I) + Send + Sync>;

fn decode_any<E: Entity>(a: &Attrs) -> Result<Box<dyn Any + Send + Sync>> {
    Ok(Box::new(E::decode(a)?))
}

/// One write of a transaction, made by a builder's `tx_op`.
pub struct TxWriteOp {
    build: Build,
    decode: DecodeAny,
}

impl TxWriteOp {
    pub(crate) fn new<E: Entity>(
        build: impl Fn(bool) -> Result<EngineRequest<'static>> + Send + Sync + 'static,
    ) -> Self {
        TxWriteOp {
            build: Box::new(build),
            decode: decode_any::<E>,
        }
    }
}

/// A transaction condition check on one item.
pub struct TxCheck<E: Entity> {
    b: Base<E>,
    key: Encoded,
}

impl<E: Entity> TxCheck<E> {
    /// Adds the conditions to check.
    pub fn where_(mut self, cond: Cond) -> Self {
        self.b.conds.push(cond);
        self
    }

    /// Overrides the table for this op.
    pub fn table(mut self, name: impl Into<String>) -> Self {
        self.b.o.table = Some(name.into());
        self
    }

    /// Makes the check a transaction op.
    pub fn tx_op(mut self) -> TxWriteOp {
        self.b.opts.transaction = true;
        TxWriteOp::new::<E>(move |exec| {
            let key = take(&self.key)?;
            self.b.build(Write::check(self.b.t.model(), &key), exec)
        })
    }

    /// Makes the check a transaction op that returns the existing item when
    /// its condition fails.
    pub fn tx_op_return_old(mut self) -> TxWriteOp {
        self.b.opts.response = "all_old".into();
        self.tx_op()
    }
}

impl<E: Entity> Table<E> {
    /// A transaction condition check on one item.
    pub fn check(&self, key: E::Key) -> TxCheck<E> {
        TxCheck {
            b: Base::new(self),
            key: encoded(E::encode_key(&key)),
        }
    }
}

/// One cancellation reason: its code, message and old item.
type Reason = (String, String, Option<Attrs>);

/// The transaction's error, when DynamoDB canceled it.
fn canceled(err: Error) -> std::result::Result<(SdkError, Vec<Reason>), Error> {
    let Error::Sdk(e) = err else { return Err(err) };
    let reasons = match &*e {
        SdkError::TransactionCanceledException(tce) => tce
            .cancellation_reasons()
            .iter()
            .map(|r| {
                (
                    r.code().unwrap_or_default().to_string(),
                    r.message().unwrap_or_default().to_string(),
                    r.item().map(from_item),
                )
            })
            .collect(),
        _ => return Err(Error::Sdk(e)),
    };
    Ok((*e, reasons))
}

/// A write transaction, applied all or nothing.
pub struct TxWrite {
    cfg: Config,
    ops: Vec<TxWriteOp>,
    token: Option<String>,
    client: Option<Arc<dyn DynamoDb>>,
    mutate: Vec<Mutator<TransactWriteItemsInput>>,
}

/// Starts a write transaction of `ops`.
pub fn tx_write(cfg: &Config, ops: impl IntoIterator<Item = TxWriteOp>) -> TxWrite {
    TxWrite::new(cfg, ops)
}

impl TxWrite {
    pub fn new(cfg: &Config, ops: impl IntoIterator<Item = TxWriteOp>) -> Self {
        TxWrite {
            cfg: cfg.clone(),
            ops: ops.into_iter().collect(),
            token: None,
            client: None,
            mutate: Vec::new(),
        }
    }

    /// Adds an op.
    pub fn op(mut self, op: TxWriteOp) -> Self {
        self.ops.push(op);
        self
    }

    /// Sets the ClientRequestToken, making retries idempotent.
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Overrides the DynamoDB client.
    pub fn client(mut self, client: impl DynamoDb) -> Self {
        self.client = Some(Arc::new(client));
        self
    }

    /// Edits the request before it's sent.
    pub fn mutate(
        mut self,
        f: impl Fn(&mut TransactWriteItemsInput) + Send + Sync + 'static,
    ) -> Self {
        self.mutate.push(Arc::new(f));
        self
    }

    fn request(
        &self,
        exec: bool,
    ) -> Result<(Vec<EngineRequest<'static>>, TransactWriteItemsInput)> {
        let mut reqs = Vec::with_capacity(self.ops.len());
        let mut items = Vec::with_capacity(self.ops.len());
        for (i, op) in self.ops.iter().enumerate() {
            let r = (op.build)(exec)
                .map_err(|e| Error::Invalid(format!("wattdb: transaction op {i}: {e}")))?;
            items.push(Sdk::from(&r.params)?.tx_write_item(r.target)?);
            reqs.push(r);
        }
        let mut input = TransactWriteItemsInput::builder()
            .set_transact_items(Some(items))
            .set_client_request_token(self.token.clone())
            .build()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok((reqs, input))
    }

    /// The request, without sending it.
    pub fn params(&self) -> Result<TransactWriteItemsInput> {
        Ok(self.request(false)?.1)
    }

    /// Runs the transaction. A canceled transaction is
    /// `Error::TxCanceled`, with one reason per op.
    pub async fn exec(self) -> Result<()> {
        let (reqs, input) = self.request(true)?;
        if reqs.is_empty() {
            return Ok(());
        }
        let err = match self
            .cfg
            .send(
                None,
                self.client.as_ref(),
                Request::TransactWriteItems(input),
            )
            .await
        {
            Ok(r) => return r.transact_write_items().map(|_| ()),
            Err(e) => e,
        };
        let (source, raw) = canceled(err)?;
        let mut reasons = Vec::with_capacity(reqs.len());
        for (i, r) in reqs.iter().enumerate() {
            let (code, msg, item) = raw.get(i).cloned().unwrap_or_default();
            let res = r.tx_canceled_result(&code, &msg, item.as_ref())?;
            let item = res
                .item
                .as_ref()
                .map(|d| (self.ops[i].decode)(d))
                .transpose()?;
            reasons.push(TxReason::new(res.code, res.message, res.rejected, item));
        }
        Err(Error::TxCanceled(Box::new(TxCanceledError {
            reasons,
            source,
        })))
    }
}

/// One read of a read transaction, made by a get's `tx_get`.
pub struct TxGetOp<E: Entity> {
    get: Get<E>,
}

impl<E: Entity> TxGetOp<E> {
    pub(crate) fn new(get: Get<E>) -> Self {
        TxGetOp { get }
    }
}

/// Where a read transaction's item for an op comes back.
pub struct TxSlot<E> {
    index: usize,
    _e: PhantomData<fn() -> E>,
}

impl<E> Clone for TxSlot<E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E> Copy for TxSlot<E> {}

/// A read transaction: a consistent read of every op's item.
pub struct TxGet {
    cfg: Config,
    ops: Vec<Build>,
    client: Option<Arc<dyn DynamoDb>>,
    mutate: Vec<Mutator<TransactGetItemsInput>>,
}

impl TxGet {
    pub fn new(cfg: &Config) -> Self {
        TxGet {
            cfg: cfg.clone(),
            ops: Vec::new(),
            client: None,
            mutate: Vec::new(),
        }
    }

    /// Adds an op, returning where its item comes back.
    pub fn add<E: Entity>(&mut self, op: TxGetOp<E>) -> TxSlot<E> {
        let get = op.get;
        self.ops
            .push(Box::new(move |exec| get.request(exec).map(|(r, _)| r)));
        TxSlot {
            index: self.ops.len() - 1,
            _e: PhantomData,
        }
    }

    /// Overrides the DynamoDB client.
    pub fn client(mut self, client: impl DynamoDb) -> Self {
        self.client = Some(Arc::new(client));
        self
    }

    /// Edits the request before it's sent.
    pub fn mutate(
        mut self,
        f: impl Fn(&mut TransactGetItemsInput) + Send + Sync + 'static,
    ) -> Self {
        self.mutate.push(Arc::new(f));
        self
    }

    fn request(&self, exec: bool) -> Result<(Vec<EngineRequest<'static>>, TransactGetItemsInput)> {
        let mut reqs = Vec::with_capacity(self.ops.len());
        let mut items = Vec::with_capacity(self.ops.len());
        for (i, op) in self.ops.iter().enumerate() {
            let r =
                op(exec).map_err(|e| Error::Invalid(format!("wattdb: transaction op {i}: {e}")))?;
            items.push(Sdk::from(&r.params)?.tx_get_item()?);
            reqs.push(r);
        }
        let mut input = TransactGetItemsInput::builder()
            .set_transact_items(Some(items))
            .build()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok((reqs, input))
    }

    /// The request, without sending it.
    pub fn params(&self) -> Result<TransactGetItemsInput> {
        Ok(self.request(false)?.1)
    }

    /// Runs the transaction.
    pub async fn exec(self) -> Result<TxGetOutput> {
        let (reqs, input) = self.request(true)?;
        if reqs.is_empty() {
            return Ok(TxGetOutput { items: Vec::new() });
        }
        let out = match self
            .cfg
            .send(None, self.client.as_ref(), Request::TransactGetItems(input))
            .await
        {
            Ok(r) => r.transact_get_items()?,
            Err(err) => {
                let (source, raw) = canceled(err)?;
                let reasons = (0..reqs.len())
                    .map(|i| {
                        let (code, msg, _) = raw.get(i).cloned().unwrap_or_default();
                        let code = if code.is_empty() {
                            "None".to_string()
                        } else {
                            code
                        };
                        let rejected = code != "None";
                        TxReason::new(code, msg, rejected, None)
                    })
                    .collect();
                return Err(Error::TxCanceled(Box::new(TxCanceledError {
                    reasons,
                    source,
                })));
            }
        };
        let responses = out.responses.unwrap_or_default();
        let mut items = Vec::with_capacity(reqs.len());
        for (i, r) in reqs.iter().enumerate() {
            let item = responses
                .get(i)
                .and_then(|resp| resp.item.as_ref())
                .map(from_item);
            items.push(r.tx_get_result(item.as_ref())?.item);
        }
        Ok(TxGetOutput { items })
    }
}

/// A read transaction's items.
pub struct TxGetOutput {
    items: Vec<Option<Attrs>>,
}

impl TxGetOutput {
    /// The item for an op; `None` when it doesn't exist (or isn't the
    /// entity's).
    pub fn get<E: Entity>(&self, slot: TxSlot<E>) -> Result<Option<E>> {
        self.items
            .get(slot.index)
            .and_then(Option::as_ref)
            .map(E::decode)
            .transpose()
    }
}
