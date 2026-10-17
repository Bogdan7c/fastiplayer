//! Смена формата decoded PCM посреди потока: пересборка audio output-а без потери A/V sync.
//!
//! # Когда срабатывает
//!
//! Decoder любого кодека (контракт `audio_core::AudioDecoder` нейтральный) вернул PCM,
//! чей `AudioOutputSpec` (частота или раскладка каналов) отличается от spec-а активного
//! output-а. Типичный пример — вещательный AC-3: реклама в 2.0, фильм в 5.1. Раньше это
//! было фатальной ошибкой; теперь `ensure_audio_output_for_decoded_spec` зовёт сюда.
//! Тот же spec — ничего не происходит (никаких лишних пересозданий).
//!
//! # Что происходит с уже принятым PCM старого формата
//!
//! Выбрано «доиграть, а не выбросить»: старый output не закрывается, а доигрывает свой
//! хвост параллельно с новым (см. `pipeline/overlapped_audio_output.rs`). Новый output
//! получает тишину ровно длиной этого хвоста, затем PCM нового формата. Итог:
//! - звук на стыке не вырезается (в отличие от C3-пересоздания, где хвост теряется);
//! - первый PCM нового формата звучит в момент, когда у старого кончился хвост —
//!   т.е. ровно в своё media-время;
//! - видео не ждёт и не прыгает.
//!
//! # Clock
//!
//! media-время = якорь + позиция clock-а. Позиция стыка и длина хвоста берутся из
//! **одного** чтения clock-а старого output-а (`capture_audio_clock_mapping`). Якорь
//! нового clock-а — его стартовая позиция до `play` (а не «текущая» после установки),
//! поэтому тишина длиной хвоста переводит clock ровно в `стык + хвост × rate`.
//! Если скорость сменилась незадолго до стыка, хвост ещё содержит PCM прежней скорости:
//! новому clock-у передаётся вся кусочная история старого mapping-а с точки стыка
//! (`SpecChangeTimeline::ContinuePreviousMapping`), а не один якорь с текущей скоростью —
//! иначе позиция уехала бы на `хвост × Δrate`.
//!
//! # Скорость ≠ 1.0x
//!
//! Tempo processor привязан к PCM-формату. Перед стыком его DSP-хвост (уже принятый
//! вход старого формата) дописывается в **старый** output тем же `finish`, что и на EOF,
//! processor закрывается, и следующий PCM нового формата создаёт свежий processor с
//! новым форматом — как после seek (без warmup-истории: история старого формата к
//! новому не подходит).
//!
//! # Состояния и ошибки
//!
//! - Новый output запускается, только если старый играл; иначе (пауза, buffering,
//!   ещё не запущен) он ждёт обычного `play`, который продолжит оба потока.
//! - Устройство — то же, где играл старый output: смена формата не должна ни
//!   возвращать звук на выбранное устройство после аварийного перехода на default,
//!   ни заново показывать уведомление о переходе.
//! - Бюджет восстановления после потери устройства не тратится и не сбрасывается:
//!   это не поломка потока.
//! - Пользовательских событий нет (это не новое устройство и не новый media),
//!   только диагностический `info!`.
//! - Не удалось создать/заполнить/запустить новый output → `AudioDeviceUnavailable`,
//!   как при ленивом создании: звук выключается, видео продолжает играть с той же
//!   позиции на monotonic clock-е. Ошибка DSP при сбросе tempo-хвоста — runtime-ошибка
//!   (политика tempo: после commit отката нет).

use std::time::{Duration, Instant};

use tracing::info;

use crate::pipeline::{ReplacementOutputPlayback, SpecChangeClockAnchor, SpecChangeTimeline};
use crate::{
    AudioOutputDeviceRequest, AudioOutputDeviceRoute, AudioOutputSpec, CreatedAudioOutput,
    PlayerAudioOutput, PlayerError, PlayerErrorKind, PlayerResult,
};

use super::PlayerSession;
use super::audio_output_handover::prefill_replacement_output_with_silence;

/// На каком устройстве играет активный output (по последнему фактическому маршруту).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ActiveAudioOutputPlacement {
    /// Устройство, выбранное в настройках (или output ещё не создавался).
    #[default]
    SelectedDevice,

    /// Системное устройство по умолчанию после запасного маршрута или восстановления.
    SystemDefault,
}

impl ActiveAudioOutputPlacement {
    /// Переводит фактический маршрут factory в «где сейчас играем».
    fn from_route(route: &AudioOutputDeviceRoute) -> Self {
        match route {
            AudioOutputDeviceRoute::SelectedDevice => Self::SelectedDevice,
            AudioOutputDeviceRoute::SystemDefault
            | AudioOutputDeviceRoute::SystemDefaultInsteadOfUnavailable { .. } => {
                Self::SystemDefault
            }
        }
    }

    /// Какое устройство просить для output-а нового формата.
    ///
    /// На выбранном устройстве разрешён обычный запасной маршрут (если устройство
    /// пропало именно сейчас, это настоящая новость для пользователя). Уже на default —
    /// остаёмся там: автоматического возврата на выбранное устройство нет (бэклог).
    const fn device_request_for_format_change(self) -> AudioOutputDeviceRequest {
        match self {
            Self::SelectedDevice => AudioOutputDeviceRequest::SelectedOrSystemDefault,
            Self::SystemDefault => AudioOutputDeviceRequest::SystemDefault,
        }
    }
}

/// Точка стыка старого output-а: что звучит сейчас и сколько принятого ещё не прозвучало.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SpecChangeHandoverPoint {
    /// Media-время слышимого сейчас звука.
    media_position: Duration,

    /// PCM старого формата, принятый, но ещё не проигранный (доиграет старый output).
    unplayed_tail: Duration,

    /// Как media-время продолжается на оси нового clock-а.
    timeline: SpecChangeTimeline,
}

impl PlayerSession {
    /// Запоминает, на каком устройстве теперь играет созданный output.
    pub(super) fn remember_audio_output_placement(&mut self, route: &AudioOutputDeviceRoute) {
        self.audio_output_placement = ActiveAudioOutputPlacement::from_route(route);
    }

    /// Пересобирает output под новый decoded spec; правила — в документации модуля.
    ///
    /// При `AudioDeviceUnavailable` звук уже выключен, а видео переведено на monotonic
    /// clock с позиции стыка; вызывающий код публикует recoverable ошибку как обычно.
    pub(super) fn rebuild_audio_output_for_decoded_spec_change(
        &mut self,
        active_spec: AudioOutputSpec,
        decoded_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
        let observed_at = Instant::now();
        let position_before_change = self.presentation_clock_position_at(observed_at);
        let rebuild_result = self.replace_audio_output_for_decoded_spec(active_spec, decoded_spec);
        if let Err(error) = &rebuild_result
            && error.kind == PlayerErrorKind::AudioDeviceUnavailable
        {
            self.continue_video_without_audio(position_before_change, observed_at);
        }
        rebuild_result
    }

    /// Создаёт output нового формата и атомарно ставит его вместо старого.
    ///
    /// Все шаги, которые могут не удаться на новом устройстве, выполняются до
    /// перестановки слотов: при ошибке старый output, clock и mapping не тронуты.
    fn replace_audio_output_for_decoded_spec(
        &mut self,
        active_spec: AudioOutputSpec,
        decoded_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
        ensure_complete_decoded_spec(decoded_spec)?;

        let device_request = self
            .audio_output_placement
            .device_request_for_format_change();
        let CreatedAudioOutput {
            output: mut replacement_output,
            route,
        } = self
            .audio_output_factory
            .create_output(decoded_spec, device_request)
            .map_err(|error| {
                spec_change_device_error(&format!("new output creation failed: {error}"))
            })?;
        replacement_output.set_volume(self.snapshot.volume);

        // DSP-хвост старого формата дописывается в старый output до снятия точки стыка,
        // чтобы его длительность вошла в хвост и в тишину нового output-а.
        self.flush_audio_tempo_processor_for_eof()?;
        let handover_point = self.capture_spec_change_handover_point();

        let silence_frames = prefill_replacement_output_with_silence(
            replacement_output.as_mut(),
            decoded_spec,
            handover_point.unplayed_tail,
        )?;
        let output_clock_start = replacement_output
            .clock()
            .output_timing()
            .audible_output_position();
        let playback = self.start_replacement_output_like_previous(replacement_output.as_mut())?;

        self.pipeline.install_audio_output_after_spec_change(
            replacement_output,
            decoded_spec,
            SpecChangeClockAnchor {
                output_clock_start,
                media_position: handover_point.media_position,
                timeline: handover_point.timeline,
            },
            playback,
        );
        self.pipeline
            .reset_audio_clock_sample(self.audio_clock_now(), Instant::now());
        self.remember_audio_output_placement(&route);
        self.publish_audio_output_route(route);

        info!(
            active_sample_rate = active_spec.sample_rate,
            active_channel_layout = %active_spec.channel_layout,
            decoded_sample_rate = decoded_spec.sample_rate,
            decoded_channel_layout = %decoded_spec.channel_layout,
            handover_media_position_ms = handover_point.media_position.as_secs_f64() * 1_000.0,
            unplayed_tail_ms = handover_point.unplayed_tail.as_secs_f64() * 1_000.0,
            silence_frames,
            ?playback,
            "Audio output rebuilt for a mid-stream decoded format change; the previous output plays out its tail"
        );
        Ok(())
    }

    /// Снимает позицию стыка и хвост старого output-а одним чтением clock-а.
    ///
    /// Clock-а нет только у тестового output-а без spec-а; тогда хвоста нет, а позиция —
    /// последняя опубликованная.
    fn capture_spec_change_handover_point(&self) -> SpecChangeHandoverPoint {
        match self.pipeline.capture_audio_clock_mapping() {
            Some(captured_clock) => SpecChangeHandoverPoint {
                media_position: captured_clock.media_position(),
                unplayed_tail: captured_clock.unplayed_output_tail(),
                timeline: SpecChangeTimeline::ContinuePreviousMapping {
                    previous_output_clock: captured_clock.audible_output_clock(),
                },
            },
            None => SpecChangeHandoverPoint {
                media_position: self.current_source_position,
                unplayed_tail: Duration::ZERO,
                timeline: SpecChangeTimeline::StartAtPlaybackRate(self.snapshot.playback_rate),
            },
        }
    }

    /// Запускает новый output, только если старый уже играл.
    fn start_replacement_output_like_previous(
        &self,
        replacement_output: &mut dyn PlayerAudioOutput,
    ) -> PlayerResult<ReplacementOutputPlayback> {
        if !self.pipeline.audio_output_is_playing() {
            return Ok(ReplacementOutputPlayback::NotStarted);
        }
        replacement_output.play().map_err(|error| {
            spec_change_device_error(&format!("new output failed to start: {error}"))
        })?;
        Ok(ReplacementOutputPlayback::Started)
    }

    /// Выключает звук после неудачной пересборки, не останавливая видео.
    ///
    /// Тот же исход, что у исчерпанного восстановления устройства: без audio clock-а
    /// видео идёт по monotonic clock-у с позиции, которая звучала до стыка.
    fn continue_video_without_audio(&mut self, position: Duration, observed_at: Instant) {
        self.disable_selected_audio_path();
        self.reanchor_no_audio_clock(position, observed_at);
    }
}

/// Проверяет, что decoder сообщил полный spec (частота и число каналов не нулевые).
fn ensure_complete_decoded_spec(decoded_spec: AudioOutputSpec) -> PlayerResult<()> {
    let sample_rate = decoded_spec.sample_rate;
    let channels = decoded_spec.channels();
    if sample_rate == 0 || channels == 0 {
        return Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!(
                "Audio decoder changed to an incomplete AudioSpec: sample_rate={sample_rate}, channels={channels}"
            ),
        ));
    }
    Ok(())
}

/// Ошибка нового output-а: тот же вид, что у остальных отказов устройства.
fn spec_change_device_error(detail: &str) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::AudioDeviceUnavailable,
        format!("Audio output rebuild for a new audio format failed: {detail}"),
    )
}
