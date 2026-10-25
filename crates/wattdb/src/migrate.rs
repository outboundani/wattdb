//! Migrations between schema versions (decision D47).

use std::time::Duration;

use futures::StreamExt;

use crate::batch::{Backoff, MAX_BATCH_WRITE};
use crate::entity::{Entity, Table};
use crate::error::{Error, Result};

/// Copies every `Old` item into the `New` version: it scans `src`, converts
/// each item with `transform`, and writes the results to `dst` with batch
/// puts (retrying unprocessed items up to 8 times from 50ms). It returns how
/// many items it wrote. The old items are left in place; delete them once
/// nothing reads them.
///
/// ElectroDB treats each version as a separate entity, as wattdb does, so a
/// migration runs from one version to the next: v1 to v3 runs v1 to v2,
/// then v2 to v3.
pub async fn migrate<Old: Entity, New: Entity>(
    src: &Table<Old>,
    dst: &Table<New>,
    mut transform: impl FnMut(Old) -> Result<New::Input>,
) -> Result<usize> {
    let mut written = 0;
    let mut batch: Vec<New::Input> = Vec::with_capacity(MAX_BATCH_WRITE);
    let flush = |batch: Vec<New::Input>| async move {
        let n = batch.len();
        let left = dst
            .batch_put(batch)
            .retry_unprocessed(Backoff::new(8, Duration::from_millis(50)))
            .exec()
            .await?;
        if !left.is_empty() {
            return Err(Error::Invalid(format!(
                "wattdb: migration: {} items left unprocessed",
                left.len()
            )));
        }
        Ok::<usize, Error>(n)
    };
    let mut items = std::pin::pin!(src.scan().all());
    while let Some(item) = items.next().await {
        batch.push(transform(item?)?);
        if batch.len() == MAX_BATCH_WRITE {
            written += flush(std::mem::take(&mut batch)).await?;
        }
    }
    if !batch.is_empty() {
        written += flush(batch).await?;
    }
    Ok(written)
}
