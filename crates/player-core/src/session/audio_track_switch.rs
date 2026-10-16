//! Переключение аудиодорожки во время воспроизведения («тихий seek», как в mpv).
//!
//! # Владелец и границы
//! Решение и порядок шагов принадлежат session; физический сброс audio-слотов — pipeline
//! (`PlaybackPipeline::replace_selected_audio_track`); перепозиционирование — существующая
//! seek-машинерия (`PlayerSession::seek`). Video decoder, субтитры, громкость и playback
//! rate эта операция не трогает.
//!
//! # Семантика команды `SelectAudioTrack(id)`
//! - `id` не существует в текущем media или это не аудиодорожка -> `InvalidCommand`
//!   (recoverable), выбор/snapshot/события не меняются.
//! - `id` уже выбран -> no-op: ни сброса decoder-а, ни seek-а, ни события.
//! - иначе: сброс аудио-пути + новый deferred decoder config по метаданным новой дорожки
//!   (свой кодек, частота, каналы; output пересоздаётся по первому decoded spec),
//!   обновление snapshot + событие `AudioTrackSelected`, затем перепозиционирование.
//!
//! # Перепозиционирование ([`AudioTrackSwitchResume`])
//! Старые аудио-пакеты в demux-буфере уже прочитаны «вперёд» по старой дорожке, а
//! декодер новой дорожки обязан стартовать с её пакетов. Поэтому в обычном случае
//! делается внутренний accurate seek в текущую позицию (тот же путь, что и
//! `reseek_to_current_position_after_backend_swap` для смены video backend-а): demux
//! перезапускается, поколение пакетов растёт (хвост старой дорожки отбрасывается),
//! A/V sync выставляется заново, а `resume_intent` seek-а берётся из текущего состояния —
//! Playing остаётся Playing, Paused остаётся Paused. Seek виден как короткий
//! `Seeking`/`SeekRequested` — это сознательная цена точной синхронизации (решение
//! владельца, сессия 07 плана субтитров).
//!
//! Если seek невозможен (источник не seekable, нет demuxer-а) звук продолжается с
//! ближайших прочитанных пакетов новой дорожки без seek-а: часть звука между текущей
//! позицией и demux-курсором теряется, но воспроизведение не падает и не зависает.
//! Если seek или scrub уже идёт — он сам владеет позицией и часами, и новая дорожка
//! подхватывается его потоком пакетов без второго seek-а.

use std::time::Instant;

use media_core::{TrackId, TrackInfo, TrackKind};
use tracing::{debug, info, warn};

use super::PlayerSession;
use super::audio_runtime::audio_decoder_init_spec_from_track;
use crate::{PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult, SeekRequest};

/// Как после сброса аудио-пути восстанавливается позиция воспроизведения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioTrackSwitchResume {
    /// Запущен внутренний accurate seek в текущую позицию.
    SeekToCurrentPosition,
    /// Seek невозможен (не seekable / нет demuxer-а): звук идёт со следующих пакетов.
    ContinueForwardWithoutSeek,
    /// Уже идёт seek/scrub: он владеет позицией, второй seek не запускается.
    JoinInFlightSeek,
}

/// Итог команды выбора аудиодорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioTrackSwitchOutcome {
    /// Дорожка уже выбрана: ничего не менялось.
    AlreadySelected,
    /// Аудио-путь пересоздан под новую дорожку, позиция восстановлена указанным способом.
    Switched(AudioTrackSwitchResume),
}

impl PlayerSession {
    /// Применяет выбор аудиодорожки (см. семантику в документации модуля).
    pub(super) fn switch_selected_audio_track(
        &mut self,
        track_id: TrackId,
    ) -> PlayerResult<AudioTrackSwitchOutcome> {
        let decoder_config = match self.plan_audio_track_switch(track_id)? {
            AudioTrackSwitchPlan::AlreadySelected => {
                debug!(
                    ?track_id,
                    "Audio track is already selected; switch is a no-op"
                );
                return Ok(AudioTrackSwitchOutcome::AlreadySelected);
            }
            AudioTrackSwitchPlan::Replace(decoder_config) => decoder_config,
        };

        info!(?track_id, "Switching audio track: audio path is reset");
        self.pipeline
            .replace_selected_audio_track(track_id, decoder_config);
        self.snapshot.selected_tracks.audio_track = Some(track_id);
        self.push_player_event(PlayerEvent::AudioTrackSelected(track_id));

        Ok(AudioTrackSwitchOutcome::Switched(
            self.restore_position_after_audio_track_switch(),
        ))
    }

    /// Проверяет `track_id` и готовит decoder config новой дорожки, ничего не меняя.
    fn plan_audio_track_switch(&mut self, track_id: TrackId) -> PlayerResult<AudioTrackSwitchPlan> {
        let Some(track) = self.find_audio_track(track_id) else {
            let error = PlayerError::new(
                PlayerErrorKind::InvalidCommand,
                format!("audio track {track_id:?} does not exist in the current media"),
            );
            self.record_recoverable_error(error.clone());
            return Err(error);
        };

        if self.pipeline.selected_audio_track_id() == Some(track_id) {
            return Ok(AudioTrackSwitchPlan::AlreadySelected);
        }

        match audio_decoder_config_for_track(&track) {
            Ok(decoder_config) => Ok(AudioTrackSwitchPlan::Replace(decoder_config)),
            Err(error) => {
                self.record_recoverable_error(error.clone());
                Err(error)
            }
        }
    }

    /// Возвращает аудиодорожку текущего media по id (видео/субтитры не подходят).
    fn find_audio_track(&self, track_id: TrackId) -> Option<TrackInfo> {
        self.pipeline
            .tracks()
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Audio)
            .cloned()
    }

    /// Восстанавливает позицию после сброса аудио-пути выбранным способом.
    fn restore_position_after_audio_track_switch(&mut self) -> AudioTrackSwitchResume {
        if self.snapshot.timeline.seeking || self.snapshot.timeline.scrubbing {
            return AudioTrackSwitchResume::JoinInFlightSeek;
        }

        if self.snapshot.timeline.seekable && self.pipeline.has_demuxer() {
            match self.seek(SeekRequest::accurate(self.current_source_position)) {
                Ok(()) => return AudioTrackSwitchResume::SeekToCurrentPosition,
                Err(error) => warn!(
                    error = %error,
                    "Seek after audio track switch was rejected; continuing without seek"
                ),
            }
        }

        self.continue_playback_clock_without_audio_output();
        AudioTrackSwitchResume::ContinueForwardWithoutSeek
    }

    /// Держит media clock на месте, пока у нового аудио-пути ещё нет output/clock.
    ///
    /// Старый audio clock удалён вместе с output-ом; без якоря позиция при Playing
    /// остановилась бы до первого декодированного пакета новой дорожки. Якорь
    /// снимается автоматически, когда новый output устанавливает свой clock.
    fn continue_playback_clock_without_audio_output(&mut self) {
        let position = self.current_source_position;
        self.pipeline.set_media_clock_base(position);
        self.reanchor_no_audio_clock(position, Instant::now());
    }
}

/// План замены аудиодорожки, вычисленный до любых изменений состояния.
enum AudioTrackSwitchPlan {
    /// Выбрана та же дорожка.
    AlreadySelected,
    /// Нужно заменить аудио-путь decoder config-ом новой дорожки.
    Replace(audio_core::AudioDecoderConfig),
}

/// Строит decoder config по метаданным дорожки (как при открытии media).
fn audio_decoder_config_for_track(
    track: &TrackInfo,
) -> PlayerResult<audio_core::AudioDecoderConfig> {
    let init_spec = audio_decoder_init_spec_from_track(track)?;
    Ok(audio_core::AudioDecoderConfig::from_track_metadata(
        init_spec.track_id.get(),
        init_spec.codec_id,
        init_spec.initial_sample_rate,
        init_spec.initial_channels,
    )
    .with_codec_private(init_spec.codec_private))
}
