//! Conversions between composites, keys and cursors (decision D45).

use std::marker::PhantomData;

use wattdb_engine::conv as engine;
pub use wattdb_engine::conv::Strict;
use wattdb_engine::model::Index;

use crate::entity::{Entity, Table};
use crate::error::{Error, Result};
use crate::query::Cursor;
use crate::value::{Item, from_item, to_item};

/// Converts between composite attributes, keys and cursors across every
/// index. Composites are the entity's `Partial`.
pub struct Conv<E: Entity> {
    m: &'static wattdb_engine::Model,
    _e: PhantomData<fn() -> E>,
}

impl<E: Entity> Clone for Conv<E> {
    fn clone(&self) -> Self {
        Conv {
            m: self.m,
            _e: PhantomData,
        }
    }
}

impl<E: Entity> Table<E> {
    /// The entity's conversions.
    pub fn conv(&self) -> Conv<E> {
        Conv {
            m: self.model(),
            _e: PhantomData,
        }
    }
}

fn cursor(c: Option<String>) -> Option<Cursor> {
    c.map(Cursor::new)
}

impl<E: Entity> Conv<E> {
    /// Narrows the conversions to one access pattern (and the table's
    /// keys).
    pub fn index(&self, access_pattern: &str) -> Result<IndexConv<E>> {
        let ix = self.m.access_pattern(access_pattern).ok_or_else(|| {
            Error::Invalid(format!("wattdb: unknown access pattern {access_pattern:?}"))
        })?;
        Ok(IndexConv {
            m: self.m,
            ix,
            _e: PhantomData,
        })
    }

    /// Builds every index's keys that the composites allow.
    pub fn keys_from_composite(&self, composite: &E::Partial, strict: Strict) -> Result<Item> {
        to_item(&engine::composite_to_keys(
            self.m,
            &E::encode_partial(composite)?,
            strict,
        )?)
    }

    /// Builds a cursor from every composite.
    pub fn cursor_from_composite(&self, composite: &E::Partial) -> Result<Option<Cursor>> {
        Ok(cursor(engine::composite_to_cursor(
            self.m,
            &E::encode_partial(composite)?,
        )?))
    }

    /// Encodes keys as a cursor.
    pub fn cursor_from_keys(&self, keys: &Item) -> Result<Option<Cursor>> {
        Ok(cursor(engine::keys_to_cursor(&from_item(keys))?))
    }

    /// Decodes a cursor's keys.
    pub fn keys_from_cursor(&self, c: &Cursor) -> Result<Item> {
        to_item(&engine::cursor_to_keys(c.as_str())?)
    }

    /// Reads the composites back out of every index's keys.
    pub fn composite_from_keys(&self, keys: &Item) -> Result<E::Partial> {
        E::decode_partial(&engine::keys_to_composite(self.m, &from_item(keys))?)
    }

    /// Reads the composites back out of a cursor.
    pub fn composite_from_cursor(&self, c: &Cursor) -> Result<E::Partial> {
        E::decode_partial(&engine::cursor_to_composite(self.m, c.as_str())?)
    }
}

/// Converts between composites, keys and cursors of one access pattern
/// (and the table's keys).
pub struct IndexConv<E: Entity> {
    m: &'static wattdb_engine::Model,
    ix: &'static Index,
    _e: PhantomData<fn() -> E>,
}

impl<E: Entity> IndexConv<E> {
    /// Builds the index's keys from the composites.
    pub fn keys_from_composite(&self, composite: &E::Partial, strict: Strict) -> Result<Item> {
        let keys = engine::composite_to_keys_by_index(
            self.m,
            self.ix,
            &E::encode_partial(composite)?,
            strict,
        )?;
        to_item(&keys.unwrap_or_default())
    }

    /// Builds the index's cursor from every composite.
    pub fn cursor_from_composite(&self, composite: &E::Partial) -> Result<Option<Cursor>> {
        Ok(cursor(engine::composite_to_cursor_by_index(
            self.m,
            self.ix,
            &E::encode_partial(composite)?,
        )?))
    }

    /// Encodes the index's keys as a cursor.
    pub fn cursor_from_keys(&self, keys: &Item) -> Result<Option<Cursor>> {
        Ok(cursor(engine::keys_to_cursor_by_index(
            self.m,
            self.ix,
            &from_item(keys),
        )?))
    }

    /// Reads the composites back out of the index's keys.
    pub fn composite_from_keys(&self, keys: &Item) -> Result<E::Partial> {
        E::decode_partial(&engine::keys_to_composite_by_index(
            self.m,
            self.ix,
            &from_item(keys),
        )?)
    }

    /// Reads the composites back out of the index's cursor.
    pub fn composite_from_cursor(&self, c: &Cursor) -> Result<E::Partial> {
        E::decode_partial(&engine::cursor_to_composite_by_index(
            self.m,
            self.ix,
            c.as_str(),
        )?)
    }
}
