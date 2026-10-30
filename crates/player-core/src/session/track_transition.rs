//! Смена описания дорожек посреди потока без сброса плеера («эстафета», сессия 03d моста).
//!
//! Demuxer сообщает `TracksChanged`, когда у выбранной дорожки меняется описание:
//! смена качества (новые SPS/PPS, разрешение), смена кодека (`changeType` у MSE),
//! новый период DASH или эпоха HLS. Разбор идёт впереди показа, поэтому прежний
//! немедленный сброс выбрасывал кадры и звук, уже разобранные впрок, а при звуке,
//! разобранном далеко вперёд видео, плеер мог навсегда зависнуть в доигрывании.
//!
//! Теперь смена — передача палочки на границе показа:
//! - список дорожек и длительность обновляются сразу (UI видит новое качество);
//! - новое описание видео или звука получает номер в `PlaybackPipeline`, и все
//!   packet-ы после него помечены этим номером;
//! - видео: старый decoder доигрывает packet-ы до границы, выжимается до
//!   последнего кадра и только потом перенастраивается (`tick/video_track_handoff.rs`);
//! - звук: decoder меняется прямо перед первым packet-ом нового описания, а выход
//!   звука и часы не трогаются (смену формата PCM подхватывает пересборка выхода
//!   `audio_spec_change`).
//!
//! Старый сброс (`handle_demux_track_list_update`) остаётся там, где эстафета
//! неприменима: идёт перемотка или скраб (у них свои правила маркеров
//! `TracksChanged`), изменился состав дорожек, новое видео требует другой backend
//! или не играется, описание звука не принимается, слишком много ждущих смен.

use std::time::Duration;

use media_core::{TrackId, TrackInfo, TrackKind};
use tracing::{info, warn};

use crate::PlayerResult;
use crate::pipeline::TrackDescriptionAdmission;

use super::PlayerSession;
use super::audio_runtime::{AudioDecoderInitSpec, audio_decoder_init_spec_from_track};

/// Как session применила `TracksChanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrackListUpdateHandling {
    /// Применено без сброса: decoders перейдут на новые описания на границе показа.
    AppliedWithoutReset,

    /// Нужен прежний полный сброс; причина — для журнала и тестов.
    RequiresReset(TrackListResetReason),
}

/// Почему смена дорожек идёт старым сбросом, а не эстафетой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrackListResetReason {
    /// Идёт перемотка, скраб или ждёт квитанция seek-а.
    SeekInProgress,

    /// Ждёт установка другого video backend-а.
    BackendReselectionPending,

    /// Изменился состав видео- или аудиодорожек либо выбранная дорожка.
    TrackTopologyChanged,

    /// Новое описание видео не принимает активный decoder.
    VideoDecoderRejectsDescription,

    /// Новое описание звука не превращается в конфигурацию decoder-а.
    AudioDescriptionRejected,

    /// Слишком много смен ждут, пока decoder дойдёт до границы.
    TooManyPendingDescriptions,
}

/// Какие описания выбранных дорожек поменялись.
#[derive(Debug, Clone, PartialEq)]
enum SelectedDescriptionChange {
    /// Видео- и аудиодорожки те же; изменённые описания выбранных — внутри.
    SameTopology(Box<ChangedSelectedDescriptions>),

    /// Состав дорожек другой: эстафета не знает, кому передавать палочку.
    TopologyChanged,
}

/// Новые описания выбранных дорожек; `None` — описание этого вида не менялось.
#[derive(Debug, Clone, PartialEq)]
struct ChangedSelectedDescriptions {
    video: Option<TrackInfo>,
    audio: Option<TrackInfo>,
}

/// Дорожки, выбранные сейчас в pipeline.
#[derive(Debug, Clone, Copy)]
struct SelectedTrackIds {
    video: Option<TrackId>,
    audio: Option<TrackId>,
}

impl PlayerSession {
    /// Пытается применить `TracksChanged` эстафетой, без сброса decoders и очередей.
    ///
    /// Ничего не меняет, если возвращает `RequiresReset`: тогда caller идёт
    /// прежним путём полного сброса.
    pub(super) fn apply_track_list_update_without_reset(
        &mut self,
        tracks: &[TrackInfo],
        duration: Option<Duration>,
    ) -> TrackListUpdateHandling {
        if let Some(reason) = self.track_list_update_blocker() {
            return TrackListUpdateHandling::RequiresReset(reason);
        }
        let selected = SelectedTrackIds {
            video: self.pipeline.selected_video_track_id(),
            audio: self.pipeline.selected_audio_track_id(),
        };
        let SelectedDescriptionChange::SameTopology(changed) =
            classify_selected_description_change(self.pipeline.tracks(), tracks, selected)
        else {
            return TrackListUpdateHandling::RequiresReset(
                TrackListResetReason::TrackTopologyChanged,
            );
        };
        let ChangedSelectedDescriptions { video, audio } = *changed;
        if let Some(reason) = self.track_description_rejection(video.as_ref(), audio.as_ref()) {
            return TrackListUpdateHandling::RequiresReset(reason);
        }

        // Дальше — только мутации; все проверки выше их не делали.
        self.pipeline
            .replace_track_list_keeping_decoders(tracks.to_vec());
        self.set_snapshot_duration(duration);
        if let Some(video_track) = video {
            self.admit_video_description(video_track);
        }
        if let Some(audio_track) = audio {
            self.admit_audio_description(audio_track);
        }
        TrackListUpdateHandling::AppliedWithoutReset
    }

    /// Сбрасывает video decoder перед seek (flush — fail-fast граница) и сразу
    /// настраивает его на последнее описание дорожки.
    ///
    /// Flush выбросил всё старое, а packet-ы после seek-а относятся к последнему
    /// принятому описанию. Поэтому decoder настраивается на него сразу, без
    /// выжимания, — до того, как seek поставит decoder-side preroll floor.
    pub(super) fn configure_video_decoder_for_latest_track_description(
        &mut self,
    ) -> PlayerResult<()> {
        let Some(track) = self.pipeline.settle_video_track_descriptions_after_flush() else {
            return Ok(());
        };
        info!(
            track_id = %track.id,
            codec_id = %track.codec_id,
            "Flushed video decoder configured for the latest track description"
        );
        self.select_default_video_track(
            std::slice::from_ref(&track),
            "Video track description disappeared after decoder flush",
        )?;
        Ok(())
    }

    /// Меняет audio decoder перед первым packet-ом нового описания звука.
    ///
    /// Выход звука, его буфер и часы не трогаются: PCM старого описания доигрывает,
    /// а смену формата PCM подхватывает `ensure_audio_output_for_decoded_spec`.
    pub(super) fn apply_audio_track_description_for_packet(
        &mut self,
        packet_description: crate::pipeline::TrackDescriptionEpoch,
    ) {
        let Some(track) = self
            .pipeline
            .take_audio_track_description_for_packet(packet_description)
        else {
            return;
        };
        self.pipeline.clear_audio_decoder();
        match audio_decoder_init_spec_from_track(&track) {
            Ok(init_spec) => {
                info!(
                    track_id = %init_spec.track_id,
                    codec_id = %init_spec.codec_id,
                    "Audio decoder handed over to the next track description"
                );
                self.pipeline
                    .install_deferred_audio_decoder_config(deferred_audio_decoder_config(
                        &init_spec,
                    ));
            }
            Err(error) => {
                // Описание проверялось при `TracksChanged`; сюда попадает только
                // нарушение этого договора. Звук нового описания не играет, видео — да.
                warn!(error = %error, "Audio track description rejected at handover");
                self.pipeline.clear_deferred_audio_decoder_config();
                self.record_recoverable_error(error);
            }
        }
    }

    /// Причина, по которой эстафета сейчас невозможна в принципе.
    fn track_list_update_blocker(&self) -> Option<TrackListResetReason> {
        if self.seek_runtime.active_commit().is_some()
            || self.prepared_demux_seek.receipt_pending()
            || self.seek_runtime.active_seek_landing_is_live_scrub()
        {
            return Some(TrackListResetReason::SeekInProgress);
        }
        if self.has_pending_video_backend_reselection() {
            return Some(TrackListResetReason::BackendReselectionPending);
        }
        None
    }

    /// Проверяет изменённые описания без мутаций.
    fn track_description_rejection(
        &self,
        video: Option<&TrackInfo>,
        audio: Option<&TrackInfo>,
    ) -> Option<TrackListResetReason> {
        if video.is_some() && !self.pipeline.can_admit_video_track_description()
            || audio.is_some() && !self.pipeline.can_admit_audio_track_description()
        {
            return Some(TrackListResetReason::TooManyPendingDescriptions);
        }
        if let Some(video_track) = video
            && !self.active_video_decoder_accepts_track(video_track)
        {
            return Some(TrackListResetReason::VideoDecoderRejectsDescription);
        }
        if let Some(audio_track) = audio
            && audio_decoder_init_spec_from_track(audio_track).is_err()
        {
            return Some(TrackListResetReason::AudioDescriptionRejected);
        }
        None
    }

    fn admit_video_description(&mut self, track: TrackInfo) {
        let track_id = track.id;
        match self.pipeline.admit_video_track_description(track) {
            TrackDescriptionAdmission::Admitted(epoch) => info!(
                track_id = %track_id,
                description = ?epoch,
                "Video track description queued for decoder handoff at the presentation boundary"
            ),
            // Место проверено в `track_description_rejection` до мутаций.
            TrackDescriptionAdmission::QueueFull => warn!(
                track_id = %track_id,
                "Video track description queue unexpectedly full; description ignored"
            ),
        }
    }

    fn admit_audio_description(&mut self, track: TrackInfo) {
        let track_id = track.id;
        match self.pipeline.admit_audio_track_description(track) {
            TrackDescriptionAdmission::Admitted(epoch) => info!(
                track_id = %track_id,
                description = ?epoch,
                "Audio track description queued for decoder handover at its first packet"
            ),
            TrackDescriptionAdmission::QueueFull => warn!(
                track_id = %track_id,
                "Audio track description queue unexpectedly full; description ignored"
            ),
        }
    }
}

/// Конфигурация audio decoder-а, создаваемого лениво по первому packet-у.
pub(super) fn deferred_audio_decoder_config(
    init_spec: &AudioDecoderInitSpec,
) -> audio_core::AudioDecoderConfig {
    audio_core::AudioDecoderConfig::from_track_metadata(
        init_spec.track_id.get(),
        init_spec.codec_id.clone(),
        init_spec.initial_sample_rate,
        init_spec.initial_channels,
    )
    .with_codec_private(init_spec.codec_private.clone())
}

/// Сравнивает старый и новый списки для выбранных видео и звука.
fn classify_selected_description_change(
    previous: &[TrackInfo],
    next: &[TrackInfo],
    selected: SelectedTrackIds,
) -> SelectedDescriptionChange {
    if decodable_topology(previous) != decodable_topology(next)
        || !selection_covers_kind(previous, selected.video, TrackKind::Video)
        || !selection_covers_kind(previous, selected.audio, TrackKind::Audio)
    {
        return SelectedDescriptionChange::TopologyChanged;
    }
    SelectedDescriptionChange::SameTopology(Box::new(ChangedSelectedDescriptions {
        video: changed_description(previous, next, selected.video),
        audio: changed_description(previous, next, selected.audio),
    }))
}

/// Идентичности и виды видео- и аудиодорожек по порядку; субтитры не участвуют.
fn decodable_topology(tracks: &[TrackInfo]) -> Vec<(TrackId, TrackKind)> {
    tracks
        .iter()
        .filter(|track| matches!(track.kind, TrackKind::Video | TrackKind::Audio))
        .map(|track| (track.id, track.kind))
        .collect()
}

/// Выбор согласован со списком: дорожка вида есть ⇔ одна из них выбрана.
///
/// Есть дорожки, но ничего не выбрано (звук отключён ошибкой, видео ждёт
/// backend) — эстафете нечего передавать; прежний сброс выбирает заново.
fn selection_covers_kind(tracks: &[TrackInfo], selected: Option<TrackId>, kind: TrackKind) -> bool {
    let has_kind = tracks.iter().any(|track| track.kind == kind);
    match selected {
        Some(track_id) => tracks
            .iter()
            .any(|track| track.id == track_id && track.kind == kind),
        None => !has_kind,
    }
}

/// Новое описание выбранной дорожки, если оно отличается тем, что видит decoder.
fn changed_description(
    previous: &[TrackInfo],
    next: &[TrackInfo],
    selected: Option<TrackId>,
) -> Option<TrackInfo> {
    let track_id = selected?;
    let previous_track = previous.iter().find(|track| track.id == track_id)?;
    let next_track = next.iter().find(|track| track.id == track_id)?;
    (!same_decoder_description(previous_track, next_track)).then(|| next_track.clone())
}

/// Совпадает ли всё, из чего строится конфигурация decoder-а.
///
/// Длительность и метки (язык, название) на decoder не влияют: их смена не
/// повод выжимать decoder.
fn same_decoder_description(previous: &TrackInfo, next: &TrackInfo) -> bool {
    previous.codec_id == next.codec_id
        && previous.codec_private == next.codec_private
        && previous.time_base == next.time_base
        && previous.sample_rate == next.sample_rate
        && previous.channels == next.channels
        && previous.video == next.video
}

#[cfg(test)]
#[path = "track_transition/classification_tests.rs"]
mod classification_tests;
