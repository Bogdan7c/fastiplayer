//! Audio output на время смены формата звука: новый поток играет, старый доигрывает хвост.
//!
//! # Зачем
//!
//! Decoder посреди потока сменил формат PCM (вещательный AC-3 2.0 → 5.1, другая частота).
//! Старый output открыт под прежний формат и уже принял ~200 мс PCM, которые ещё не
//! прозвучали. Забрать их из кольца нельзя (consumer живёт в callback-е потока), а
//! выбросить — значит вырезать кусок звука на стыке.
//!
//! Поэтому session открывает новый output и пишет в него тишину ровно длиной этого
//! хвоста, а старый output **не закрывается**: он доигрывает свой хвост параллельно с
//! тишиной нового потока (звуковой сервер смешивает оба потока). Когда хвост прозвучал,
//! новый поток как раз доходит до первого настоящего PCM нового формата — без дыры и
//! без сдвига A/V.
//!
//! # Владение и инварианты
//!
//! - Этот тип — обычный `PlayerAudioOutput` в слоте pipeline, поэтому весь существующий
//!   lifecycle (seek, смена media, пересоздание при потере устройства, очистка) работает
//!   без отдельных веток: уходит слот — уходят оба потока.
//! - Clock, уровень буфера, здоровье потока и запись PCM принадлежат **только новому**
//!   output-у. Старый поток не влияет на позицию и не получает новый PCM.
//! - Управление (play/pause/громкость) применяется к обоим, пока старый жив: пауза во
//!   время стыка замораживает и хвост, Play продолжает оба синхронно.
//! - Старый output отпускается, когда его хвост прозвучал, когда его поток сломался или
//!   при очистке буфера для seek (хвост после перемотки уже не нужен). Его ошибки не
//!   становятся ошибками player-а: хвост — best effort, синхрон держит тишина нового.

use std::sync::Arc;

use audio_core::{
    AudioOutputClockTiming, AudioOutputStreamHealth, AudioOutputWriteError, AudioOutputWriteIntent,
    AudioOutputWriteReport, PlayerAudioClock, PlayerAudioOutput,
};
use tracing::{debug, warn};

/// Output нового формата вместе со старым output-ом, доигрывающим свой хвост.
pub(super) struct OutputWithRetiringTail {
    /// Output нового формата: владеет clock-ом и принимает весь PCM после смены.
    active: Box<dyn PlayerAudioOutput>,

    /// Output прежнего формата; `None` — хвост доигран и поток уже закрыт.
    retiring: Option<Box<dyn PlayerAudioOutput>>,
}

/// Что сейчас происходит с хвостом старого output-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetiringTailState {
    /// Принятый PCM ещё звучит — поток нужно держать открытым.
    StillPlaying,

    /// Весь принятый PCM прозвучал — поток можно закрыть.
    TailPlayed,

    /// Backend сообщил о поломке потока — хвост уже не прозвучит.
    StreamFailed,
}

/// Почему старый output отпущен (только для диагностического лога).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetiringOutputRelease {
    /// Хвост доигран.
    TailPlayed,

    /// Поток старого output-а сломался.
    StreamFailed,

    /// Seek очистил буфер: хвост до перемотки больше не нужен.
    SeekClearedBuffer,

    /// Старый поток не смог выполнить play/pause вместе с новым.
    ControlFailed,
}

impl OutputWithRetiringTail {
    /// Объединяет новый output и старый, который доигрывает принятый хвост.
    pub(super) fn new(
        active: Box<dyn PlayerAudioOutput>,
        retiring: Box<dyn PlayerAudioOutput>,
    ) -> Self {
        Self {
            active,
            retiring: Some(retiring),
        }
    }

    /// Закрывает старый output, если его хвост прозвучал или поток умер.
    ///
    /// Вызывается на каждой записи PCM: во время воспроизведения запись идёт постоянно,
    /// поэтому старый поток закрывается вскоре после конца хвоста.
    fn release_retiring_output_if_finished(&mut self) {
        let Some(retiring) = self.retiring.as_deref() else {
            return;
        };
        match retiring_tail_state(retiring) {
            RetiringTailState::StillPlaying => {}
            RetiringTailState::TailPlayed => {
                self.release_retiring_output(RetiringOutputRelease::TailPlayed);
            }
            RetiringTailState::StreamFailed => {
                self.release_retiring_output(RetiringOutputRelease::StreamFailed);
            }
        }
    }

    /// Закрывает старый output и пишет причину в лог.
    fn release_retiring_output(&mut self, reason: RetiringOutputRelease) {
        if self.retiring.take().is_some() {
            debug!(
                ?reason,
                "The previous audio output is released after a format change"
            );
        }
    }
}

/// Классифицирует хвост старого output-а по его собственному clock-у и здоровью потока.
fn retiring_tail_state(output: &dyn PlayerAudioOutput) -> RetiringTailState {
    if matches!(output.stream_health(), AudioOutputStreamHealth::Failed) {
        return RetiringTailState::StreamFailed;
    }
    if output
        .clock()
        .output_timing()
        .submitted_output_tail()
        .is_zero()
    {
        RetiringTailState::TailPlayed
    } else {
        RetiringTailState::StillPlaying
    }
}

impl PlayerAudioOutput for OutputWithRetiringTail {
    /// Новый PCM всегда идёт только в новый output.
    fn write_samples(
        &mut self,
        samples: &[f32],
        intent: AudioOutputWriteIntent,
    ) -> Result<AudioOutputWriteReport, AudioOutputWriteError> {
        self.release_retiring_output_if_finished();
        self.active.write_samples(samples, intent)
    }

    /// Запускает новый поток и продолжает хвост старого.
    ///
    /// Ошибка нового потока возвращается как раньше; ошибка старого только закрывает
    /// его (синхрон держит тишина нового output-а, теряется лишь остаток хвоста).
    fn play(&mut self) -> anyhow::Result<()> {
        self.active.play()?;
        if let Some(retiring) = self.retiring.as_mut()
            && let Err(error) = retiring.play()
        {
            warn!(error = %error, "The previous audio output could not resume its tail");
            self.release_retiring_output(RetiringOutputRelease::ControlFailed);
        }
        Ok(())
    }

    /// Ставит на паузу оба потока; clock и timing — нового output-а.
    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        let output_timing = self.active.pause_and_freeze_clock()?;
        if let Some(retiring) = self.retiring.as_mut()
            && let Err(error) = retiring.pause_and_freeze_clock()
        {
            warn!(error = %error, "The previous audio output could not pause its tail");
            self.release_retiring_output(RetiringOutputRelease::ControlFailed);
        }
        Ok(output_timing)
    }

    /// Seek очищает буфер нового output-а и закрывает старый: его хвост — до перемотки.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.release_retiring_output(RetiringOutputRelease::SeekClearedBuffer);
        self.active.clear_buffer_for_seek(generation)
    }

    /// Громкость применяется к обоим потокам: mute во время стыка глушит и хвост.
    fn set_volume(&mut self, volume: f32) {
        self.active.set_volume(volume);
        if let Some(retiring) = self.retiring.as_mut() {
            retiring.set_volume(volume);
        }
    }

    /// Уровень буфера нового output-а (в нём тишина длиной хвоста + новый PCM).
    fn buffer_level_ms(&self) -> f64 {
        self.active.buffer_level_ms()
    }

    /// Clock нового output-а — единственный источник позиции звука.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.active.clock()
    }

    /// Здоровье нового потока; поломка старого обрабатывается здесь же, без recovery.
    fn stream_health(&self) -> AudioOutputStreamHealth {
        self.active.stream_health()
    }
}
