//! Engine errors (decision D48).

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

/// A boxed error returned by a schema callback.
pub type BoxError = Box<dyn StdError + Send + Sync>;

/// An engine error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// Invalid attribute values.
    #[error(transparent)]
    Validation(#[from] ValidationError),
    /// A key or index key couldn't be built from what was given.
    #[error(transparent)]
    IncompleteComposites(#[from] IncompleteCompositesError),
    /// A cursor that can't be decoded.
    #[error("wattdb: invalid cursor: {0}")]
    InvalidCursor(String),
    /// A conversion's input doesn't fit the entity.
    #[error("wattdb: invalid conversion input: {0}")]
    Conversion(String),
    /// A schema callback failed.
    #[error("{0}")]
    Callback(#[source] CallbackError),
    /// Anything else ElectroDB rejects: an invalid model, operation or
    /// option.
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    /// Wraps an error returned by a callback.
    pub fn callback(err: impl Into<BoxError>) -> Self {
        Error::Callback(CallbackError(Arc::from(err.into())))
    }
}

/// An error returned by a schema callback, shareable so engine errors stay
/// cloneable.
#[derive(Clone)]
pub struct CallbackError(pub Arc<dyn StdError + Send + Sync>);

impl fmt::Debug for CallbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for CallbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl StdError for CallbackError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&*self.0)
    }
}

/// Key composites a write or lookup needs but didn't get.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IncompleteCompositesError {
    /// The affected access patterns, if known.
    pub access_patterns: Vec<String>,
    pub missing: Vec<String>,
    /// Set when a sparse index (one with a condition) was given some but
    /// not all of its composites.
    pub conditional: bool,
}

impl fmt::Display for IncompleteCompositesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "wattdb: missing composite attributes {}",
            self.missing.join(", ")
        )?;
        if !self.access_patterns.is_empty() {
            write!(
                f,
                " for access patterns {}",
                self.access_patterns.join(", ")
            )?;
        }
        Ok(())
    }
}

impl StdError for IncompleteCompositesError {}

/// One invalid attribute value.
#[derive(Debug, Clone)]
pub struct FieldError {
    pub path: String,
    pub reason: String,
    /// The list index the error occurred at, if any.
    pub index: Option<usize>,
    /// The error a validate callback returned, if any.
    pub cause: Option<CallbackError>,
}

/// Invalid attribute values.
#[derive(Debug, Clone, Default)]
pub struct ValidationError {
    pub fields: Vec<FieldError>,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self
            .fields
            .iter()
            .map(|e| format!("{}: {}", e.path, e.reason))
            .collect();
        write!(f, "wattdb: invalid attributes: {}", parts.join("; "))
    }
}

impl StdError for ValidationError {}

pub type Result<T, E = Error> = std::result::Result<T, E>;
