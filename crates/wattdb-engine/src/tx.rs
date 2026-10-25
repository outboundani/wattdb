//! Transactions (ElectroDB's transaction.js).

use crate::error::Result;
use crate::item::{FormatOptions, format_item, owns_item};
use crate::request::TxParams;
use crate::value::Attrs;
use crate::write::Request;

/// Renders TransactWriteItems or TransactGetItems params from requests
/// built with `Options::transaction`.
pub fn tx_params(reqs: &[Request<'_>], token: Option<&str>) -> TxParams {
    TxParams {
        items: reqs.iter().map(|r| (r.target, r.params.clone())).collect(),
        token: token.filter(|t| !t.is_empty()).map(String::from),
    }
}

/// One transaction item's outcome, as ElectroDB reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TxResult {
    /// The formatted item: a get's item, or the old item a failed condition
    /// returned; `None` when absent or not the entity's.
    pub item: Option<Attrs>,
    pub code: String,
    pub message: String,
    pub rejected: bool,
}

impl TxResult {
    /// A committed write item's result.
    pub fn write() -> Self {
        TxResult {
            code: "None".into(),
            ..Default::default()
        }
    }
}

impl Request<'_> {
    /// Mirrors `cleanseTransactionData` for one item.
    pub fn tx_get_result(&self, item: Option<&Attrs>) -> Result<TxResult> {
        let Some(item) = item else {
            return Ok(TxResult::default());
        };
        Ok(TxResult {
            item: self.tx_item(item)?,
            ..Default::default()
        })
    }

    /// Mirrors `cleanseCanceledData` for one cancellation reason.
    pub fn tx_canceled_result(
        &self,
        code: &str,
        message: &str,
        item: Option<&Attrs>,
    ) -> Result<TxResult> {
        let code = if code.is_empty() { "None" } else { code };
        let mut res = TxResult {
            code: code.into(),
            message: message.into(),
            rejected: code != "None",
            item: None,
        };
        if let Some(item) = item {
            res.item = self.tx_item(item)?;
        }
        Ok(res)
    }

    fn tx_item(&self, item: &Attrs) -> Result<Option<Attrs>> {
        let m = self.model();
        if !owns_item(m, item) {
            return Ok(None);
        }
        let o = self.options();
        Ok(Some(format_item(
            m,
            item,
            &FormatOptions {
                include_keys: o.include_keys,
                attributes: o.attributes.clone(),
            },
        )?))
    }
}
