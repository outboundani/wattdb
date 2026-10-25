//! The schema-driven ElectroDB 3.9 engine behind wattdb (decision D23).
//!
//! It works on untyped [`Value`]s keyed by attribute name and reproduces
//! ElectroDB's keys, items, expressions, queries, cursors, batches,
//! transactions, collections and conversions byte for byte. The typed
//! layer in the `wattdb` crate converts Rust types to and from these values
//! and sends the requests it builds.

pub mod batch;
pub mod conv;
pub mod error;
mod expr;
pub mod item;
pub mod keys;
pub mod model;
mod ops;
pub mod pager;
pub mod query;
pub mod request;
pub mod tx;
pub mod value;
pub mod wire;
pub mod write;

pub use error::{Error, FieldError, IncompleteCompositesError, Result, ValidationError};
pub use model::Model;
pub use ops::Path;
pub use value::{Attrs, Number, Value};
