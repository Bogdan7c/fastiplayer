//! Единая composition boundary `PreparedMedia` для web adapter-ов.
//!
//! Любой web opener (yt-dlp candidate, native HLS/DASH/Smooth/HDS) отдаёт сюда
//! demuxer и runtime-only attachments; функция собирает player-facing
//! `PreparedMedia` до strong install barrier-а. Ошибки timeline mode и initial
//! position остаются различимыми, чтобы вызывающий код мог трактовать их по-разному.

use std::sync::Arc;

use media_core::{Demuxer, DynamicMediaTimelinePort};
use player_core::{PreparedDemuxSeekPort, PreparedMedia};

/// Named seek attachment сохраняет обычную и authoritative landing semantics.
pub enum PreparedWebMediaSeekAttachment {
    WorkerReceipted(Arc<dyn PreparedDemuxSeekPort>),
    AuthoritativePostTarget(Arc<dyn PreparedDemuxSeekPort>),
}

/// Runtime-only attachments, которые устанавливаются до strong barrier-а.
#[derive(Default)]
pub struct PreparedWebMediaAttachments {
    pub timeline_port: Option<DynamicMediaTimelinePort>,
    pub demux_seek: Option<PreparedWebMediaSeekAttachment>,
    pub playback_window: Option<player_core::MediaPlaybackWindow>,
    pub initial_position: Option<player_core::PreparedInitialPosition>,
}

/// Composition сохраняет различимые ошибки timeline mode и initial position.
#[derive(Debug, thiserror::Error)]
pub enum PreparedWebMediaCompositionError {
    #[error(transparent)]
    TimelineMode(#[from] player_core::PreparedMediaTimelineModeError),
    #[error(transparent)]
    InitialPosition(#[from] player_core::PreparedInitialPositionError),
}

/// Собирает один player-facing `PreparedMedia` для любого web adapter-а.
pub fn compose_prepared_web_media(
    safe_label: &str,
    demuxer: Box<dyn Demuxer + Send>,
    attachments: PreparedWebMediaAttachments,
) -> Result<PreparedMedia, PreparedWebMediaCompositionError> {
    let mut prepared_media = PreparedMedia::from_external_label(safe_label, demuxer);
    if let Some(seek_attachment) = attachments.demux_seek {
        prepared_media = match seek_attachment {
            PreparedWebMediaSeekAttachment::WorkerReceipted(port) => {
                prepared_media.with_worker_receipted_demux_seek(port)
            }
            PreparedWebMediaSeekAttachment::AuthoritativePostTarget(port) => prepared_media
                .with_worker_receipted_demux_seek_policy(
                    port,
                    player_core::PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget,
                ),
        };
    }
    if let Some(playback_window) = attachments.playback_window {
        prepared_media = prepared_media.with_playback_window(playback_window)?;
    }
    prepared_media = match attachments.timeline_port {
        Some(timeline_port) => prepared_media.with_dynamic_timeline(timeline_port),
        None => Ok(prepared_media),
    }?;
    match attachments.initial_position {
        Some(initial_position) => {
            Ok(prepared_media.with_prepared_initial_position(initial_position)?)
        }
        None => Ok(prepared_media),
    }
}
