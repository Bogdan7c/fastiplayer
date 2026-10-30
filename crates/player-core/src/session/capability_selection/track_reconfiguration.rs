//! Перенастройка уже работающего video decoder-а на новое описание того же трека.
//!
//! Нужна эстафете смены качества (`tick/video_track_handoff.rs`): decoder выжат и
//! сброшен, теперь он получает новое описание. В отличие от выбора трека при
//! открытии media, здесь важны исходы, после которых воспроизведение идёт
//! дальше: временно полный канал управления decoder-а — повтор на следующем
//! tick-е, а не фатальная ошибка; нужен другой backend — запрос его установки.

use media_core::TrackInfo;
use tracing::debug;
use video_core::VideoStreamConfigResult;

use crate::PlayerResult;

use super::{
    PlayerSession, VideoTrackSelectionOutcome, player_result_from_stream_config_result,
    video_stream_decode_config_from_track,
};

/// Итог перенастройки video decoder-а на новое описание трека.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum VideoTrackReconfiguration {
    /// Decoder настроен, трек выбран с новым требованием и контрактом кадра.
    Configured,

    /// Канал управления decoder-а временно полон: ничего не изменено, повторить позже.
    ControlChannelBusy,

    /// Активный backend описание не тянет: запрошен другой, packet-ы его ждут.
    BackendReselectionRequested,
}

impl PlayerSession {
    /// Настраивает активный decoder на новое описание выбранного трека.
    ///
    /// `Err` — описание не играется вообще или decoder отказал фатально.
    pub(in crate::session) fn reconfigure_video_decoder_for_track(
        &mut self,
        track: &TrackInfo,
    ) -> PlayerResult<VideoTrackReconfiguration> {
        let selection = match self.accepted_video_selection_for_track(track)? {
            VideoTrackSelectionOutcome::Accepted(selection) => selection,
            VideoTrackSelectionOutcome::BackendReselectionRequired { requirement } => {
                self.request_video_backend_reselection(requirement, track.id);
                return Ok(VideoTrackReconfiguration::BackendReselectionRequested);
            }
        };
        let frame_contract = selection.frame_contract();
        let config =
            video_stream_decode_config_from_track(track, &selection.requirement, frame_contract)?;
        match self.pipeline.configure_video_decoder_stream(config) {
            VideoStreamConfigResult::Backpressure(reason) => {
                debug!(
                    reason = %reason,
                    track_id = %track.id,
                    "Video decoder reconfiguration deferred: control channel is busy"
                );
                return Ok(VideoTrackReconfiguration::ControlChannelBusy);
            }
            other => player_result_from_stream_config_result(other)?,
        }
        let requirement = selection.requirement.clone();
        self.select_configured_video_track(track, selection.requirement, frame_contract);
        self.note_active_video_stream_requirement(requirement, true);
        Ok(VideoTrackReconfiguration::Configured)
    }
}
