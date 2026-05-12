use std::sync::Arc;

use crossbeam_channel::Sender;

use crate::{
    DesktopCommandSink, DesktopIntegrationEvent, DesktopIntegrationResult, LatestSnapshotHandle,
};

use super::{BackendHandle, spawn_stub_backend};

/// Запускает no-op backend для unsupported Unix targets.
pub(crate) fn spawn(
    command_sink: Arc<dyn DesktopCommandSink>,
    snapshot_source: LatestSnapshotHandle,
    event_tx: Sender<DesktopIntegrationEvent>,
) -> DesktopIntegrationResult<BackendHandle> {
    spawn_stub_backend(command_sink, snapshot_source, event_tx)
}
