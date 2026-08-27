use media_core::MediaDemuxError;

use crate::seek_state::SeekDemuxRequestError;
use crate::{PlayerError, PlayerErrorKind};

/// Мапит ошибку demux seek в player error без смешивания unavailable/timeout/demux.
pub(super) fn player_error_from_demux_seek_error(error: anyhow::Error) -> PlayerError {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<MediaDemuxError>()
            .is_some_and(MediaDemuxError::is_seek_unavailable)
    }) {
        return PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek failed: {error}"),
        );
    }

    PlayerError::new(PlayerErrorKind::DemuxError, format!("Seek failed: {error}"))
}

/// Мапит ошибку выбора seek policy до mutating-части transaction-а.
pub(super) fn player_error_from_seek_demux_request_error(
    error: SeekDemuxRequestError,
) -> PlayerError {
    match error {
        SeekDemuxRequestError::UnsupportedSeekMode { mode } => PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek mode {mode:?} пока не поддерживается текущим demux contract"),
        ),
    }
}

/// Мапит failed decoder flush в typed player error без продолжения seek transaction.
pub(super) fn player_error_from_decoder_flush_error(error: anyhow::Error) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::DecoderFlushFailed,
        format!("Video decoder flush failed before seek: {error}"),
    )
}
