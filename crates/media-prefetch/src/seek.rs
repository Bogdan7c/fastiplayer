//! Foreground seek policy для sliding prefetch window и active network fetch-а.

use source_core::{Seekability, SourceError, SourceResult};

use crate::shared::PrefetchShared;

/// Применяет seek без раскрытия worker/buffer устройства наружу `media-prefetch`.
pub(crate) fn apply_prefetch_seek(
    shared: &PrefetchShared,
    seekability: &Seekability,
    logical_position: &mut u64,
    offset: u64,
) -> SourceResult<()> {
    let mut state = shared.lock_state();
    let buffered_end = state.buffer.buffered_end();
    let offset_is_buffered = state.buffer.contains(offset) || offset == buffered_end;

    if offset_is_buffered {
        state.buffer.set_cursor_within(offset);
        *logical_position = offset;
        tracing::debug!(
            offset,
            buffered_end,
            "media prefetch foreground seek остался внутри RAM window"
        );
        shared.notify_all();
        return Ok(());
    }

    if let Seekability::NotSeekable { reason } = seekability {
        if offset != *logical_position {
            return Err(SourceError::NotSeekable {
                reason: reason.clone(),
            });
        }

        return Ok(());
    }

    if state.stage_forward_seek_into_active_fetch(offset) {
        *logical_position = offset;
        shared.notify_all();
        return Ok(());
    }

    state.buffer.reset_to(offset);
    state.seek_request = Some(offset);
    state.fatal_error = None;
    // Отмена под тем же mutex-ом, что и `seek_request`, делает порядок видимым worker-у:
    // когда `inner.read` вернёт Cancelled, новый offset уже лежит в shared state.
    if let Some(active_fetch) = &state.active_fetch {
        active_fetch.cancel();
    }
    state.diagnostics.refetches = state.diagnostics.refetches.saturating_add(1);
    *logical_position = offset;
    tracing::debug!(
        offset,
        previous_buffered_end = buffered_end,
        refetches = state.diagnostics.refetches,
        "media prefetch foreground seek запросил новое RAM window"
    );
    shared.notify_all();
    Ok(())
}
