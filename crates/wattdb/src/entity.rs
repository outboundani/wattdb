//! Entities and the tables that hold them.

use std::marker::PhantomData;
use std::sync::Arc;

use wattdb_engine::Model;
use wattdb_engine::item::{FormatOptions, format_item, owns_item, put_item};
use wattdb_engine::model::Definition;

use crate::client::{Config, DynamoDb, Request, Response};
use crate::error::{Error, Result};
use crate::value::{Attrs, Item, from_item, to_item};

/// An entity: a struct deriving [`Entity`](derive@crate::Entity). The derive
/// implements this trait and generates the `Input`, `Partial` and `Key`
/// types; applications don't implement it by hand.
pub trait Entity: Sized + Send + Sync + 'static {
    /// The item to write: required attributes without a default are plain
    /// values, the rest are `Option`s (decision D17).
    type Input: Clone + Send + Sync + 'static;
    /// Every attribute as an `Option`; also the `Filter` of find/match and
    /// what conversions read back.
    type Partial: Default + Clone + Send + Sync + 'static;
    /// The table-key composites (decision D27).
    type Key: Clone + Send + Sync + 'static;

    /// The compiled schema.
    fn definition() -> Result<&'static EntityDef>;

    /// Decodes a formatted item (attribute names to values).
    fn decode(item: &Attrs) -> Result<Self>;
    fn decode_partial(item: &Attrs) -> Result<Self::Partial>;

    fn encode_input(input: &Self::Input) -> Result<Attrs>;
    fn encode_partial(partial: &Self::Partial) -> Result<Attrs>;
    fn encode_key(key: &Self::Key) -> Result<Attrs>;

    /// The stored field names in definition order.
    fn fields() -> &'static [&'static str];
}

/// A compiled entity schema.
pub struct EntityDef {
    model: Model,
}

impl EntityDef {
    /// Checks a definition the way ElectroDB checks a model and compiles
    /// it. Generated code calls it once per entity.
    pub fn compile(def: Definition) -> std::result::Result<EntityDef, String> {
        let entity = def.entity.clone();
        Model::new(def)
            .map(|model| EntityDef { model })
            .map_err(|e| format!("entity {entity:?}: {e}"))
    }

    /// The normalized model.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The item a put writes from an encoded input: defaults, validation,
    /// setters and keys.
    pub fn put_item(&self, input: &Attrs) -> Result<Attrs> {
        Ok(put_item(&self.model, input)?)
    }

    /// Formats a stored item for reading (getters, hidden attributes), or
    /// `None` when this entity doesn't own it.
    pub fn format_item(&self, item: &Attrs) -> Result<Option<Attrs>> {
        if !owns_item(&self.model, item) {
            return Ok(None);
        }
        Ok(Some(format_item(
            &self.model,
            item,
            &FormatOptions::default(),
        )?))
    }
}

/// The values every builder can override per call (decision D40).
#[derive(Clone, Default)]
#[doc(hidden)]
pub struct Overrides {
    pub(crate) client: Option<Arc<dyn DynamoDb>>,
    pub(crate) table: Option<String>,
}

/// An entity bound to a [`Config`] and a table (decision D26). Generated
/// clients deref to it.
pub struct Table<E: Entity> {
    cfg: Config,
    def: &'static EntityDef,
    table: String,
    _e: PhantomData<fn() -> E>,
}

impl<E: Entity> Clone for Table<E> {
    fn clone(&self) -> Self {
        Table {
            cfg: self.cfg.clone(),
            def: self.def,
            table: self.table.clone(),
            _e: PhantomData,
        }
    }
}

impl<E: Entity> std::fmt::Debug for Table<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Table")
            .field("entity", &self.def.model.entity)
            .field("table", &self.table)
            .finish()
    }
}

impl<E: Entity> Table<E> {
    /// A table of `E` items named `table`.
    pub fn new(cfg: &Config, table: impl Into<String>) -> Result<Self> {
        let table = table.into();
        if table.is_empty() {
            return Err(Error::Invalid("wattdb: table name is empty".into()));
        }
        Ok(Table {
            cfg: cfg.clone(),
            def: E::definition()?,
            table,
            _e: PhantomData,
        })
    }

    /// The table's name.
    pub fn name(&self) -> &str {
        &self.table
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// The compiled schema.
    pub fn definition(&self) -> &'static EntityDef {
        self.def
    }

    pub(crate) fn model(&self) -> &'static Model {
        &self.def.model
    }

    pub(crate) async fn send(&self, o: &Overrides, request: Request) -> Result<Response> {
        self.cfg
            .send(Some(&self.def.model.entity), o.client.as_ref(), request)
            .await
    }

    /// Decodes a formatted item.
    pub(crate) fn decode(&self, item: &Attrs) -> Result<E> {
        E::decode(item)
    }

    /// Formats a raw item read some other way (a stream record, a raw
    /// query), ignoring ownership unless the keys don't fit the entity
    /// (decision D46). `None` means it isn't this entity's item.
    pub fn parse(&self, item: &Item) -> Result<Option<E>> {
        use wattdb_engine::conv::{ParseInput, ParseOutput, parse};
        let out = parse(
            self.model(),
            &ParseInput::Item(from_item(item)),
            &FormatOptions::default(),
            true,
        )?;
        match out {
            ParseOutput::Item(Some(data)) => Ok(Some(E::decode(&data)?)),
            _ => Ok(None),
        }
    }

    /// The item a put of `input` would write, in SDK form.
    pub fn put_item(&self, input: &E::Input) -> Result<Item> {
        to_item(&self.def.put_item(&E::encode_input(input)?)?)
    }
}
