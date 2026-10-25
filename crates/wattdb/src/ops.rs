//! Single-item operations: get, put, create, delete, remove, update, patch
//! and upsert (decisions D27, D28, D33–D35, D37, D39, D40).

use std::sync::Arc;

use aws_sdk_dynamodb::operation::delete_item::DeleteItemInput;
use aws_sdk_dynamodb::operation::get_item::GetItemInput;
use aws_sdk_dynamodb::operation::put_item::PutItemInput;
use aws_sdk_dynamodb::operation::update_item::UpdateItemInput;
use wattdb_engine::write::{
    Entry, Options, Request as EngineRequest, Response as EngineResponse, Returned, Write,
};

use crate::attr::{
    Addable, Appendable, Cond, Deletable, Descriptor, RawArg, Subtractable, path_string, raw_args,
};
use crate::client::{DynamoDb, Request};
use crate::entity::{Entity, Overrides, Table};
use crate::error::{Error, Result, SdkError};
use crate::sdk::Sdk;
use crate::tx::{TxGetOp, TxWriteOp};
use crate::value::{Attrs, Value, from_item};

/// What `returning` gives back (decision D33).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnMode {
    /// The whole item after the write.
    AllNew,
    /// The whole item before the write.
    AllOld,
}

/// What `returning_updated` gives back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdatedMode {
    /// The updated attributes after the write.
    UpdatedNew,
    /// The updated attributes before the write.
    UpdatedOld,
}

impl ReturnMode {
    fn option(self) -> &'static str {
        match self {
            ReturnMode::AllNew => "all_new",
            ReturnMode::AllOld => "all_old",
        }
    }
}

impl UpdatedMode {
    fn option(self) -> &'static str {
        match self {
            UpdatedMode::UpdatedNew => "updated_new",
            UpdatedMode::UpdatedOld => "updated_old",
        }
    }
}

/// The result of a write made with `return_rejected` (decision D37).
#[derive(Debug)]
pub struct Outcome<T> {
    /// True when the write's condition failed.
    pub rejected: bool,
    /// For puts, the written item; with `return_rejected_with_old`, the
    /// existing item when the write was rejected.
    pub item: Option<T>,
}

/// A value encoded when the builder was created, kept until it runs.
pub(crate) type Encoded = std::result::Result<Attrs, String>;

pub(crate) fn encoded(r: Result<Attrs>) -> Encoded {
    r.map_err(|e| e.to_string())
}

pub(crate) fn take(e: &Encoded) -> Result<Attrs> {
    e.clone().map_err(Error::Encode)
}

type Mutator<I> = Arc<dyn Fn(&mut I) + Send + Sync>;

/// The state every single-item builder shares.
#[doc(hidden)]
pub struct Base<E: Entity> {
    pub(crate) t: Table<E>,
    pub(crate) o: Overrides,
    pub(crate) opts: Options,
    pub(crate) conds: Vec<Cond>,
}

impl<E: Entity> Clone for Base<E> {
    fn clone(&self) -> Self {
        Base {
            t: self.t.clone(),
            o: self.o.clone(),
            opts: self.opts.clone(),
            conds: self.conds.clone(),
        }
    }
}

impl<E: Entity> Base<E> {
    pub fn new(t: &Table<E>) -> Self {
        Base {
            t: t.clone(),
            o: Overrides::default(),
            opts: Options::default(),
            conds: Vec::new(),
        }
    }

    /// Applies the conditions and renders the request.
    pub fn build(&self, mut w: Write<'static>, exec: bool) -> Result<EngineRequest<'static>> {
        for c in &self.conds {
            w.where_(&c.engine()?);
        }
        let mut opts = self.opts.clone();
        opts.exec = exec;
        if let Some(t) = &self.o.table {
            opts.table = t.clone();
        }
        Ok(w.build(self.t.name(), opts)?)
    }

    pub fn decode(&self, r: &EngineResponse) -> Result<Option<E>> {
        r.data.as_ref().map(|d| self.t.decode(d)).transpose()
    }
}

/// The old item a failed condition sent back, if any.
pub(crate) fn condition_failure(err: &Error) -> Option<Option<Attrs>> {
    let Error::ConditionFailed(e) = err else {
        return None;
    };
    match &**e {
        SdkError::ConditionalCheckFailedException(c) => Some(c.item().map(from_item)),
        _ => Some(None),
    }
}

/// Turns a condition failure into an Outcome when return_rejected was
/// requested, and otherwise passes the error on.
fn rejected<E: Entity>(b: &Base<E>, r: &EngineRequest<'static>, err: Error) -> Result<Outcome<E>> {
    let Some(old) = condition_failure(&err) else {
        return Err(err);
    };
    if !r.reject_requested() {
        return Err(err);
    }
    let res = r.format_rejected(old.as_ref())?;
    Ok(Outcome {
        rejected: true,
        item: b.decode(&res)?,
    })
}

macro_rules! common_methods {
    ($input:ty) => {
        /// Overrides the table for this call.
        pub fn table(mut self, name: impl Into<String>) -> Self {
            self.b.o.table = Some(name.into());
            self
        }

        /// Overrides the DynamoDB client for this call.
        pub fn client(mut self, client: impl DynamoDb) -> Self {
            self.b.o.client = Some(Arc::new(client));
            self
        }

        /// Edits the request before it's sent (decision D40).
        pub fn mutate(mut self, f: impl Fn(&mut $input) + Send + Sync + 'static) -> Self {
            self.mutate.push(Arc::new(f));
            self
        }
    };
}

macro_rules! where_method {
    () => {
        /// Adds a condition; conditions are ANDed.
        pub fn where_(mut self, cond: Cond) -> Self {
            self.b.conds.push(cond);
            self
        }
    };
}

/// Reads one item by key.
pub struct Get<E: Entity> {
    b: Base<E>,
    key: Encoded,
    mutate: Vec<Mutator<GetItemInput>>,
}

impl<E: Entity> Get<E> {
    pub(crate) fn new(t: &Table<E>, key: &E::Key) -> Self {
        Get {
            b: Base::new(t),
            key: encoded(E::encode_key(key)),
            mutate: Vec::new(),
        }
    }

    common_methods!(GetItemInput);

    /// Makes the read strongly consistent.
    pub fn consistent(mut self) -> Self {
        self.b.opts.consistent = true;
        self
    }

    pub(crate) fn request(&self, exec: bool) -> Result<(EngineRequest<'static>, GetItemInput)> {
        let key = take(&self.key)?;
        let r = self.b.build(Write::get(self.b.t.model(), &key), exec)?;
        let mut input = Sdk::from(&r.params)?.get()?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok((r, input))
    }

    /// The request, without sending it (decision D39).
    pub fn params(&self) -> Result<GetItemInput> {
        Ok(self.request(false)?.1)
    }

    /// Reads the item; `None` when it doesn't exist (or isn't this
    /// entity's).
    pub async fn exec(self) -> Result<Option<E>> {
        let (r, input) = self.request(true)?;
        let out = self
            .b
            .t
            .send(&self.b.o, Request::GetItem(input))
            .await?
            .get_item()?;
        let res = r.format(&Returned {
            item: out.item.as_ref().map(from_item),
            attributes: None,
        })?;
        self.b.decode(&res)
    }

    /// Makes the get a read-transaction op.
    pub fn tx_get(self) -> TxGetOp<E> {
        TxGetOp::new(self)
    }
}

/// Writes a whole item (create when made by `create`).
pub struct Put<E: Entity> {
    b: Base<E>,
    item: Encoded,
    create: bool,
    mutate: Vec<Mutator<PutItemInput>>,
}

impl<E: Entity> Put<E> {
    pub(crate) fn new(t: &Table<E>, input: &E::Input, create: bool) -> Self {
        Put {
            b: Base::new(t),
            item: encoded(E::encode_input(input)),
            create,
            mutate: Vec::new(),
        }
    }

    common_methods!(PutItemInput);
    where_method!();

    pub(crate) fn request(&self, exec: bool) -> Result<(EngineRequest<'static>, PutItemInput)> {
        let item = take(&self.item)?;
        let r = self
            .b
            .build(Write::put(self.b.t.model(), &item, self.create), exec)?;
        let mut input = Sdk::from(&r.params)?.put()?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok((r, input))
    }

    pub fn params(&self) -> Result<PutItemInput> {
        Ok(self.request(false)?.1)
    }

    async fn attempt(&self) -> Result<(EngineRequest<'static>, Result<EngineResponse>)> {
        let (r, input) = self.request(true)?;
        let res = match self
            .b
            .t
            .send(&self.b.o, Request::PutItem(input))
            .await
            .and_then(|o| o.put_item())
        {
            Ok(out) => r
                .format(&Returned {
                    item: None,
                    attributes: out.attributes.as_ref().map(from_item),
                })
                .map_err(Error::from),
            Err(e) => Err(e),
        };
        Ok((r, res))
    }

    async fn run(&self) -> Result<(EngineRequest<'static>, EngineResponse)> {
        let (r, res) = self.attempt().await?;
        Ok((r, res?))
    }

    /// Writes the item and returns it as written.
    pub async fn exec(self) -> Result<E> {
        let (_, res) = self.run().await?;
        self.b
            .decode(&res)?
            .ok_or_else(|| Error::Invalid("wattdb: the put returned no item".into()))
    }

    /// Returns the item as it was before the write (`AllOld`) instead of
    /// the written item; `None` when there was none.
    pub fn returning(mut self, mode: ReturnMode) -> Returning<Self> {
        self.b.opts.response = mode.option().into();
        Returning(self)
    }

    /// Reports a failed condition in the Outcome instead of as
    /// `Error::ConditionFailed`.
    pub fn return_rejected(mut self) -> Rejectable<Self> {
        self.b.opts.return_rejected = "true".into();
        Rejectable(self)
    }

    /// Like `return_rejected`, also fetching the existing item.
    pub fn return_rejected_with_old(mut self) -> Rejectable<Self> {
        self.b.opts.return_rejected = "all_old".into();
        Rejectable(self)
    }

    /// Makes the put a transaction op.
    pub fn tx_op(mut self) -> TxWriteOp {
        self.b.opts.transaction = true;
        TxWriteOp::new::<E>(move |exec| self.request(exec).map(|(r, _)| r))
    }

    /// Makes the put a transaction op that returns the existing item when
    /// its condition fails.
    pub fn tx_op_return_old(mut self) -> TxWriteOp {
        self.b.opts.response = "all_old".into();
        self.tx_op()
    }
}

/// Deletes one item by key (remove when made by `remove`).
pub struct Delete<E: Entity> {
    b: Base<E>,
    key: Encoded,
    remove: bool,
    mutate: Vec<Mutator<DeleteItemInput>>,
}

impl<E: Entity> Delete<E> {
    pub(crate) fn new(t: &Table<E>, key: &E::Key, remove: bool) -> Self {
        Delete {
            b: Base::new(t),
            key: encoded(E::encode_key(key)),
            remove,
            mutate: Vec::new(),
        }
    }

    common_methods!(DeleteItemInput);
    where_method!();

    pub(crate) fn request(&self, exec: bool) -> Result<(EngineRequest<'static>, DeleteItemInput)> {
        let key = take(&self.key)?;
        let r = self
            .b
            .build(Write::delete(self.b.t.model(), &key, self.remove), exec)?;
        let mut input = Sdk::from(&r.params)?.delete()?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok((r, input))
    }

    pub fn params(&self) -> Result<DeleteItemInput> {
        Ok(self.request(false)?.1)
    }

    async fn attempt(&self) -> Result<(EngineRequest<'static>, Result<EngineResponse>)> {
        let (r, input) = self.request(true)?;
        let res = match self
            .b
            .t
            .send(&self.b.o, Request::DeleteItem(input))
            .await
            .and_then(|o| o.delete_item())
        {
            Ok(out) => r
                .format(&Returned {
                    item: None,
                    attributes: out.attributes.as_ref().map(from_item),
                })
                .map_err(Error::from),
            Err(e) => Err(e),
        };
        Ok((r, res))
    }

    async fn run(&self) -> Result<(EngineRequest<'static>, EngineResponse)> {
        let (r, res) = self.attempt().await?;
        Ok((r, res?))
    }

    /// Deletes the item.
    pub async fn exec(self) -> Result<()> {
        self.run().await.map(|_| ())
    }

    /// Returns the deleted item (`AllOld`); `None` when there was none.
    pub fn returning(mut self, mode: ReturnMode) -> Returning<Self> {
        self.b.opts.response = mode.option().into();
        Returning(self)
    }

    pub fn return_rejected(mut self) -> Rejectable<Self> {
        self.b.opts.return_rejected = "true".into();
        Rejectable(self)
    }

    pub fn return_rejected_with_old(mut self) -> Rejectable<Self> {
        self.b.opts.return_rejected = "all_old".into();
        Rejectable(self)
    }

    pub fn tx_op(mut self) -> TxWriteOp {
        self.b.opts.transaction = true;
        TxWriteOp::new::<E>(move |exec| self.request(exec).map(|(r, _)| r))
    }

    pub fn tx_op_return_old(mut self) -> TxWriteOp {
        self.b.opts.response = "all_old".into();
        self.tx_op()
    }
}

/// One recorded update call, replayed onto the engine when the request is
/// built.
#[derive(Clone)]
enum Step {
    Apply {
        op: &'static str,
        path: String,
        value: std::result::Result<Value, String>,
    },
    Remove(Vec<String>),
    Composite {
        path: String,
        value: std::result::Result<Value, String>,
    },
    Raw {
        clause: String,
        args: Vec<RawArg>,
    },
}

fn replay(w: &mut Write<'static>, steps: &[Step]) -> Result<()> {
    for s in steps {
        match s {
            Step::Apply { op, path, value } => {
                let v = value.clone().map_err(Error::Encode)?;
                w.apply(op, &[Entry::new(path.clone(), v)]);
            }
            Step::Remove(paths) => {
                w.remove(paths);
            }
            Step::Composite { path, value } => {
                let v = value.clone().map_err(Error::Encode)?;
                w.composite(&[Entry::new(path.clone(), v)]);
            }
            Step::Raw { clause, args } => {
                let clause = clause.trim();
                let Some((section, rest)) = clause.split_once(' ') else {
                    return Err(Error::Invalid(format!(
                        "wattdb: raw update {clause:?} needs a section keyword"
                    )));
                };
                let (names, values) = raw_args(args, "r");
                w.raw_update(&section.to_lowercase(), rest.trim(), &names, &values);
            }
        }
    }
    Ok(())
}

fn step_value<E, D: Descriptor<E>>(op: &'static str, d: &D, v: D::Value) -> Step {
    Step::Apply {
        op,
        path: path_string(d),
        value: d.encode(&v).map_err(|e| e.to_string()),
    }
}

macro_rules! update_ops {
    () => {
        /// Sets an attribute (`SET`).
        pub fn set<D: Descriptor<E>>(mut self, d: D, v: impl Into<D::Value>) -> Self {
            self.steps.push(step_value("set", &d, v.into()));
            self
        }

        /// Sets an attribute only if it has no value.
        pub fn if_not_exists<D: Descriptor<E>>(mut self, d: D, v: impl Into<D::Value>) -> Self {
            self.steps.push(step_value("ifNotExists", &d, v.into()));
            self
        }

        /// Adds to a number, or adds items to a set (`ADD`).
        pub fn add<D: Addable<E>>(mut self, d: D, v: impl Into<D::Value>) -> Self {
            self.steps.push(step_value("add", &d, v.into()));
            self
        }

        /// Subtracts from a number, treating a missing value as 0.
        pub fn subtract<D: Subtractable<E>>(mut self, d: D, v: impl Into<D::Value>) -> Self {
            self.steps.push(step_value("subtract", &d, v.into()));
            self
        }

        /// Appends items to a list, creating it if missing.
        pub fn append<D: Appendable<E>>(mut self, d: D, v: impl Into<D::Value>) -> Self {
            self.steps.push(step_value("append", &d, v.into()));
            self
        }
    };
}

/// Changes attributes of one item (patch when made by `patch`). Operations
/// apply in the order they're called.
pub struct Update<E: Entity> {
    b: Base<E>,
    key: Encoded,
    patch: bool,
    steps: Vec<Step>,
    mutate: Vec<Mutator<UpdateItemInput>>,
}

impl<E: Entity> Update<E> {
    pub(crate) fn new(t: &Table<E>, key: &E::Key, patch: bool) -> Self {
        Update {
            b: Base::new(t),
            key: encoded(E::encode_key(key)),
            patch,
            steps: Vec::new(),
            mutate: Vec::new(),
        }
    }

    common_methods!(UpdateItemInput);
    where_method!();
    update_ops!();

    /// Removes items from a set (`DELETE`).
    pub fn delete<D: Deletable<E>>(mut self, d: D, v: impl Into<D::Value>) -> Self {
        self.steps.push(step_value("delete", &d, v.into()));
        self
    }

    /// Removes an attribute (`REMOVE`).
    pub fn remove<D: Descriptor<E>>(mut self, d: D) -> Self {
        self.steps.push(Step::Remove(vec![path_string(&d)]));
        self
    }

    /// Supplies a key composite that isn't being set, so index keys that
    /// need it can be rebuilt; it's also added as an equality condition.
    pub fn composite<D: Descriptor<E>>(mut self, d: D, v: impl Into<D::Value>) -> Self {
        let v = v.into();
        self.steps.push(Step::Composite {
            path: path_string(&d),
            value: d.encode(&v).map_err(|e| e.to_string()),
        });
        self
    }

    /// Adds a raw update clause, starting with its section keyword (`SET`,
    /// `REMOVE`, `ADD` or `DELETE`). Names and values are numbered as in
    /// [`raw`](crate::raw) conditions but with an `r` prefix (`#rn0`,
    /// `:rv0`). Raw updates skip validation, as in ElectroDB.
    pub fn raw(
        mut self,
        clause: impl Into<String>,
        args: impl IntoIterator<Item = RawArg>,
    ) -> Self {
        self.steps.push(Step::Raw {
            clause: clause.into(),
            args: args.into_iter().collect(),
        });
        self
    }

    pub(crate) fn request(&self, exec: bool) -> Result<(EngineRequest<'static>, UpdateItemInput)> {
        let key = take(&self.key)?;
        let mut w = Write::update(self.b.t.model(), &key, self.patch);
        replay(&mut w, &self.steps)?;
        let r = self.b.build(w, exec)?;
        let mut input = Sdk::from(&r.params)?.update()?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok((r, input))
    }

    pub fn params(&self) -> Result<UpdateItemInput> {
        Ok(self.request(false)?.1)
    }

    async fn attempt(&self) -> Result<(EngineRequest<'static>, Result<EngineResponse>)> {
        let (r, input) = self.request(true)?;
        let res = match self
            .b
            .t
            .send(&self.b.o, Request::UpdateItem(input))
            .await
            .and_then(|o| o.update_item())
        {
            Ok(out) => r
                .format(&Returned {
                    item: None,
                    attributes: out.attributes.as_ref().map(from_item),
                })
                .map_err(Error::from),
            Err(e) => Err(e),
        };
        Ok((r, res))
    }

    async fn run(&self) -> Result<(EngineRequest<'static>, EngineResponse)> {
        let (r, res) = self.attempt().await?;
        Ok((r, res?))
    }

    /// Runs the update.
    pub async fn exec(self) -> Result<()> {
        self.run().await.map(|_| ())
    }

    /// Returns the whole item, before (`AllOld`) or after (`AllNew`) the
    /// update.
    pub fn returning(mut self, mode: ReturnMode) -> Returning<Self> {
        self.b.opts.response = mode.option().into();
        Returning(self)
    }

    /// Returns only the updated attributes, before or after the update.
    pub fn returning_updated(mut self, mode: UpdatedMode) -> ReturningUpdated<Self> {
        self.b.opts.response = mode.option().into();
        ReturningUpdated(self)
    }

    pub fn return_rejected(mut self) -> Rejectable<Self> {
        self.b.opts.return_rejected = "true".into();
        Rejectable(self)
    }

    pub fn return_rejected_with_old(mut self) -> Rejectable<Self> {
        self.b.opts.return_rejected = "all_old".into();
        Rejectable(self)
    }

    pub fn tx_op(mut self) -> TxWriteOp {
        self.b.opts.transaction = true;
        TxWriteOp::new::<E>(move |exec| self.request(exec).map(|(r, _)| r))
    }

    pub fn tx_op_return_old(mut self) -> TxWriteOp {
        self.b.opts.response = "all_old".into();
        self.tx_op()
    }
}

/// Writes an item's attributes, creating the item if needed, and applies
/// further operations (decision D35).
pub struct Upsert<E: Entity> {
    b: Base<E>,
    item: Encoded,
    steps: Vec<Step>,
    mutate: Vec<Mutator<UpdateItemInput>>,
}

impl<E: Entity> Upsert<E> {
    pub(crate) fn new(t: &Table<E>, input: &E::Input) -> Self {
        Upsert {
            b: Base::new(t),
            item: encoded(E::encode_input(input)),
            steps: Vec::new(),
            mutate: Vec::new(),
        }
    }

    common_methods!(UpdateItemInput);
    where_method!();
    update_ops!();

    pub(crate) fn request(&self, exec: bool) -> Result<(EngineRequest<'static>, UpdateItemInput)> {
        let item = take(&self.item)?;
        let order: Vec<String> = E::fields().iter().map(|f| f.to_string()).collect();
        let mut w = Write::upsert(self.b.t.model(), &item, &order);
        replay(&mut w, &self.steps)?;
        let r = self.b.build(w, exec)?;
        let mut input = Sdk::from(&r.params)?.update()?;
        for f in &self.mutate {
            f(&mut input);
        }
        Ok((r, input))
    }

    pub fn params(&self) -> Result<UpdateItemInput> {
        Ok(self.request(false)?.1)
    }

    async fn attempt(&self) -> Result<(EngineRequest<'static>, Result<EngineResponse>)> {
        let (r, input) = self.request(true)?;
        let res = match self
            .b
            .t
            .send(&self.b.o, Request::UpdateItem(input))
            .await
            .and_then(|o| o.update_item())
        {
            Ok(out) => r
                .format(&Returned {
                    item: None,
                    attributes: out.attributes.as_ref().map(from_item),
                })
                .map_err(Error::from),
            Err(e) => Err(e),
        };
        Ok((r, res))
    }

    async fn run(&self) -> Result<(EngineRequest<'static>, EngineResponse)> {
        let (r, res) = self.attempt().await?;
        Ok((r, res?))
    }

    /// Runs the upsert.
    pub async fn exec(self) -> Result<()> {
        self.run().await.map(|_| ())
    }

    /// Returns the whole item, before or after the upsert.
    pub fn returning(mut self, mode: ReturnMode) -> Returning<Self> {
        self.b.opts.response = mode.option().into();
        Returning(self)
    }

    pub fn return_rejected(mut self) -> Rejectable<Self> {
        self.b.opts.return_rejected = "true".into();
        Rejectable(self)
    }

    pub fn return_rejected_with_old(mut self) -> Rejectable<Self> {
        self.b.opts.return_rejected = "all_old".into();
        Rejectable(self)
    }

    pub fn tx_op(mut self) -> TxWriteOp {
        self.b.opts.transaction = true;
        TxWriteOp::new::<E>(move |exec| self.request(exec).map(|(r, _)| r))
    }

    pub fn tx_op_return_old(mut self) -> TxWriteOp {
        self.b.opts.response = "all_old".into();
        self.tx_op()
    }
}

/// A write builder `Returning` and friends can run.
#[doc(hidden)]
pub trait RunWrite: Send {
    type Entity: Entity;
    /// Whether a successful write's Outcome carries the item (puts do).
    const RETURNS_ITEM: bool;
    fn base(&self) -> &Base<Self::Entity>;
    fn run(
        &self,
    ) -> impl std::future::Future<Output = Result<(EngineRequest<'static>, EngineResponse)>> + Send;
    fn attempt(
        &self,
    ) -> impl std::future::Future<Output = Result<(EngineRequest<'static>, Result<EngineResponse>)>> + Send;
}

macro_rules! run_write {
    ($($t:ident => $returns:literal),*) => {$(
        impl<E: Entity> RunWrite for $t<E> {
            type Entity = E;
            const RETURNS_ITEM: bool = $returns;

            fn base(&self) -> &Base<E> {
                &self.b
            }

            fn run(&self) -> impl std::future::Future<Output = Result<(EngineRequest<'static>, EngineResponse)>> + Send {
                $t::run(self)
            }

            fn attempt(&self) -> impl std::future::Future<Output = Result<(EngineRequest<'static>, Result<EngineResponse>)>> + Send {
                $t::attempt(self)
            }
        }
    )*};
}

run_write!(Put => true, Delete => false, Update => false, Upsert => false);

/// A write whose `exec` returns an item (decision D33).
pub struct Returning<W>(W);

impl<W: RunWrite> Returning<W> {
    /// Runs the write; `None` when DynamoDB returned no item.
    pub async fn exec(self) -> Result<Option<W::Entity>> {
        let (_, res) = self.0.run().await?;
        self.0.base().decode(&res)
    }
}

/// A write whose `exec` returns the updated attributes.
pub struct ReturningUpdated<W>(W);

impl<W: RunWrite> ReturningUpdated<W> {
    /// Runs the write; `None` when DynamoDB returned nothing.
    pub async fn exec(self) -> Result<Option<<W::Entity as Entity>::Partial>> {
        let (_, res) = self.0.run().await?;
        res.data
            .as_ref()
            .map(<W::Entity as Entity>::decode_partial)
            .transpose()
    }
}

/// A write whose `exec` reports a failed condition in its Outcome.
pub struct Rejectable<W>(W);

impl<W: RunWrite> Rejectable<W> {
    /// Runs the write.
    pub async fn exec(self) -> Result<Outcome<W::Entity>> {
        let b = self.0.base();
        let (r, res) = self.0.attempt().await?;
        match res {
            Ok(res) if W::RETURNS_ITEM => Ok(Outcome {
                rejected: false,
                item: b.decode(&res)?,
            }),
            Ok(_) => Ok(Outcome {
                rejected: false,
                item: None,
            }),
            Err(err) => rejected(b, &r, err),
        }
    }
}
