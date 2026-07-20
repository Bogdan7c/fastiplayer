//! Доменный shuffle traversal поверх неизменного canonical order.

mod removal;
mod runtime;
mod types;

#[cfg(test)]
mod tests;

pub(super) use runtime::{ShuffleManualPreview, ShufflePreviewStep, ShuffleTraversal};
pub use types::{
    BulkRemoveError, BulkRemoveOutcome, MAX_SHUFFLE_HISTORY_ENTRIES, ShuffleHistoryCursor,
    ShuffleQueueRestoreError, ShuffleToggleError, ShuffleToggleOutcome,
    ShuffleTraversalRestoreError, ShuffleTraversalSnapshot,
};
