//! Foreground seek policy для sliding prefetch window и active network fetch-а.

use source_core::{Seekability, SourceError, SourceResult};

use crate::shared::PrefetchShared;

/// Применяет seek без раскрытия worker/buffer устройства наружу media-prefetch.
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
        // Пока worker не потребил pending seek, исходное окно остаётся authoritative.
        // Возврат parser-а в это окно supersede-ит сетевой reset без duplicate fetch-а.
        let superseded_pending_seek = state.seek_request.take().is_some();
        state.buffer.set_cursor_within(offset);
        *logical_position = offset;
        tracing::debug!(
            offset,
            buffered_end,
            superseded_pending_seek,
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

    // Окно сбрасывает только worker, когда действительно потребляет pending seek.
    // До этого foreground может вернуть cursor в ещё валидные buffered bytes.
    state.seek_request = Some(offset);
    state.fatal_error = None;
    // Отмена под тем же mutex-ом, что и seek_request, делает порядок видимым worker-у:
    // когда inner.read вернёт Cancelled, новый offset уже лежит в shared state.
    if let Some(active_fetch) = &state.active_fetch {
        active_fetch.cancel();
    }
    *logical_position = offset;
    tracing::debug!(
        offset,
        previous_buffered_end = buffered_end,
        "media prefetch foreground seek поставил новое RAM window в pending"
    );
    shared.notify_all();
    Ok(())
}
