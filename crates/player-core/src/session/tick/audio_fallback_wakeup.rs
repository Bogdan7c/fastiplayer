//! Потолок резервного (coarse) wakeup-а worker-а, пока audio output играет.
//!
//! Coarse wakeup — страховка на случай, когда planner не нашёл точного deadline-а.
//! Его production-интервал (250 мс) длиннее audio buffer-а по умолчанию (200 мс):
//! если какой-либо путь planner-а провалится в этот fallback при играющем звуке,
//! ring buffer гарантированно опустеет (так проявлялся бэклог AC-3 #19). Поэтому,
//! пока output действительно расходует буфер, fallback не длиннее половины целевого
//! буфера — второй половины хватает на сам tick и refill. Без играющего звука
//! (пауза, Buffering с замороженным output-ом, нет output-а) интервал прежний:
//! частые пробуждения там ничего не спасают и только тратят CPU.

use std::time::Duration;

use super::PlayerTickConfig;
use crate::{
    PlaybackState,
    session::{PlayerSession, audio_runtime::sanitize_audio_high_water_mark},
};

/// Какая доля целевого audio buffer-а может пройти до резервного wakeup-а.
///
/// Половина: худший случай fallback-а оставляет ещё половину буфера на tick + refill.
const AUDIO_FALLBACK_WAKEUP_BUFFER_FRACTION: f64 = 0.5;

/// Возвращает coarse interval с учётом играющего audio output-а.
///
/// `configured_interval` — интервал из worker config-а; результат никогда не длиннее него.
#[must_use]
pub(super) fn coarse_progress_interval_for_audio_runway(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    configured_interval: Duration,
) -> Duration {
    if !audio_output_is_consuming_buffer(session) {
        return configured_interval;
    }

    configured_interval.min(audio_fallback_wakeup_cap(tick_config))
}

/// Потолок fallback-а из настроенного буфера (`audio.buffer_target_ms` → high-water).
#[must_use]
fn audio_fallback_wakeup_cap(tick_config: &PlayerTickConfig) -> Duration {
    let buffer_target_ms =
        sanitize_audio_high_water_mark(tick_config.audio_buffer_high_water_mark_ms);
    Duration::from_secs_f64(buffer_target_ms * AUDIO_FALLBACK_WAKEUP_BUFFER_FRACTION / 1000.0)
}

/// Проверяет, что выбранный звук сейчас реально проигрывается и тратит ring buffer.
///
/// Нужны оба условия: output запущен (`play` принят и не снят паузой — это знает
/// pipeline) и playback state, в котором буфер расходуется: `Playing` или `Draining`
/// (хвост файла). `Buffering` и EOF до старта хвоста держат output на паузе,
/// `Paused`/`Stopped` до этого planner-а не доходят.
#[must_use]
fn audio_output_is_consuming_buffer(session: &PlayerSession) -> bool {
    session.pipeline.has_selected_audio_track()
        && session.pipeline.audio_output_is_playing()
        && matches!(
            session.playback_state(),
            PlaybackState::Playing | PlaybackState::Draining
        )
}
