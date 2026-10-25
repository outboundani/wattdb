//! Errors (decision D48): one enum, with ElectroDB's failures as variants
//! and the AWS error kept as the source.

use std::any::Any;
use std::fmt;

pub use wattdb_engine::error::{
    BoxError, CallbackError, FieldError, IncompleteCompositesError, ValidationError,
};

use crate::Item;

/// The AWS SDK's error for every DynamoDB operation.
pub type SdkError = aws_sdk_dynamodb::Error;

/// A wattdb error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A write's condition failed. The SDK error is the source.
    #[error("wattdb: condition failed")]
    ConditionFailed(#[source] Box<SdkError>),
    /// Invalid attribute values.
    #[error(transparent)]
    Validation(ValidationError),
    /// A key or index key couldn't be built from what was given.
    #[error(transparent)]
    IncompleteComposites(IncompleteCompositesError),
    /// A cursor that can't be decoded.
    #[error("wattdb: invalid cursor: {0}")]
    InvalidCursor(String),
    /// A conversion's input doesn't fit the entity.
    #[error("wattdb: invalid conversion input: {0}")]
    Conversion(String),
    /// A transaction was canceled; one reason per op.
    #[error(transparent)]
    TxCanceled(TxCanceledError),
    /// Hydrate couldn't fetch these keys. Only the item streams (`all()`)
    /// return it, after the items they could read; `exec()` and `pages()`
    /// report them in the page.
    #[error("wattdb: {} items weren't fetched", .0.len())]
    Unprocessed(Vec<Item>),
    /// The schema is invalid, as ElectroDB would reject it.
    #[error("wattdb: {0}")]
    Schema(String),
    /// A value can't be encoded for DynamoDB.
    #[error("wattdb: {0}")]
    Encode(String),
    /// A stored value can't be decoded into its Rust type.
    #[error("wattdb: {0}")]
    Decode(String),
    /// A schema callback failed.
    #[error(transparent)]
    Callback(CallbackError),
    /// A hook failed without sending the request.
    #[error(transparent)]
    Hook(BoxError),
    /// DynamoDB (or the SDK) failed.
    #[error(transparent)]
    Sdk(Box<SdkError>),
    /// Anything else ElectroDB rejects: an invalid operation or option.
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// Reports whether a write's condition failed.
    pub fn is_condition_failed(&self) -> bool {
        matches!(self, Error::ConditionFailed(_))
    }

    /// Reports whether this is a validation error.
    pub fn is_validation(&self) -> bool {
        matches!(self, Error::Validation(_))
    }

    /// The SDK error, if DynamoDB failed.
    pub fn sdk(&self) -> Option<&SdkError> {
        match self {
            Error::ConditionFailed(e) | Error::Sdk(e) => Some(e),
            Error::TxCanceled(t) => Some(&t.source),
            _ => None,
        }
    }

    pub(crate) fn decode(msg: impl fmt::Display) -> Self {
        Error::Decode(msg.to_string())
    }

    pub(crate) fn encode(msg: impl fmt::Display) -> Self {
        Error::Encode(msg.to_string())
    }

    /// Wraps an SDK error, recognizing failed conditions.
    pub fn from_sdk(err: SdkError) -> Self {
        match err {
            SdkError::ConditionalCheckFailedException(_) => Error::ConditionFailed(Box::new(err)),
            err => Error::Sdk(Box::new(err)),
        }
    }
}

impl From<wattdb_engine::Error> for Error {
    fn from(e: wattdb_engine::Error) -> Self {
        use wattdb_engine::Error as E;
        match e {
            E::Validation(v) => Error::Validation(v),
            E::IncompleteComposites(i) => Error::IncompleteComposites(i),
            E::InvalidCursor(c) => Error::InvalidCursor(c),
            E::Conversion(c) => Error::Conversion(c),
            E::Callback(c) => Error::Callback(c),
            E::Invalid(s) => Error::Invalid(s),
        }
    }
}

impl From<SdkError> for Error {
    fn from(e: SdkError) -> Self {
        Error::from_sdk(e)
    }
}

/// A wattdb result.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why one transaction op failed, from DynamoDB's CancellationReasons.
pub struct TxReason {
    /// DynamoDB's code: `"None"` for ops that didn't fail,
    /// `"ConditionalCheckFailed"`, `"TransactionConflict"`, ...
    pub code: String,
    pub message: String,
    /// True when `code` isn't `"None"`.
    pub rejected: bool,
    item: Option<Box<dyn Any + Send + Sync>>,
}

impl TxReason {
    pub(crate) fn new(
        code: String,
        message: String,
        rejected: bool,
        item: Option<Box<dyn Any + Send + Sync>>,
    ) -> Self {
        TxReason {
            code,
            message,
            rejected,
            item,
        }
    }

    /// The existing item, as the op's entity type, when the op asked for it
    /// with `tx_op_return_old` and its condition failed.
    pub fn item<E: 'static>(&self) -> Option<&E> {
        self.item.as_ref().and_then(|i| i.downcast_ref())
    }

    /// Reports whether the existing item came back.
    pub fn has_item(&self) -> bool {
        self.item.is_some()
    }
}

impl fmt::Debug for TxReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TxReason")
            .field("code", &self.code)
            .field("message", &self.message)
            .field("rejected", &self.rejected)
            .field("item", &self.item.is_some())
            .finish()
    }
}

/// A canceled transaction, with one reason per op in the order given
/// (decision D42).
#[derive(Debug)]
pub struct TxCanceledError {
    pub reasons: Vec<TxReason>,
    pub source: SdkError,
}

impl fmt::Display for TxCanceledError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let codes: Vec<&str> = self.reasons.iter().map(|r| r.code.as_str()).collect();
        write!(f, "wattdb: transaction canceled [{}]", codes.join(", "))
    }
}

impl std::error::Error for TxCanceledError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
