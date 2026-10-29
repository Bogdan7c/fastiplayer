//! Политика «сколько аудио можно читать вперёд» для demux admission и audio throttle.
//!
//! Обычно предел один — high-water mark audio buffer-а (`audio.buffer_target_ms`).
//! Но пока стартовый video preroll голоден (первого кадра ещё нет, а audio stream не
//! запущен), этот предел может заблокировать demux раньше, чем video decoder накопит
//! reorder/DPB глубину для первого кадра: у interleaved MKV 200 мс аудио набираются
//! за ~11 video packets, а H.264 с B-frames отдаёт первый кадр после 12-16. Audio при
//! этом не стартует (ждёт кадр), буфер не расходуется, demux не читает — вечный
//! `Buffering`. Поэтому на время голодного preroll-а предел временно поднимается до
//! ограниченного стартового потолка; после первого кадра снова действует обычный.
//!
//! Тот же потолок действует, пока EOF drain принадлежит первому просмотру (короткий
//! файл кончился раньше старта, 2026-10-29, решение 93 плана моста). Звуковой хвост
//! запускается только когда все считанные audio packets обработаны, а обработка
//! ждала, пока буфер опустится ниже high-water — без запущенного звука он не
//! опускается никогда. После EOF нового demux-ввода нет, поэтому поднятый предел
//! влияет только на декод уже считанных пакетов и не растит read-ahead.

use super::PlayerTickConfig;
use crate::session::{PlayerSession, audio_runtime::sanitize_audio_high_water_mark};

/// Жёсткий верхний предел стартового потолка, мс.
///
/// Кольцевой буфер audio output вмещает ~2000 мс PCM (`audio::output`). Потолок обязан
/// оставаться ниже ёмкости с запасом на последний декодированный пакет, иначе при
/// некорректном config-е ring buffer начал бы отбрасывать звук. Значение совпадает с
/// верхней границей config-ключа `audio.startup_read_ahead_cap_ms`.
const AUDIO_STARTUP_READ_AHEAD_SAFE_MAX_MS: f64 =
    fastiplayer_config::MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS as f64;

/// Какой предел audio read-ahead действует прямо сейчас и почему.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum AudioReadAheadAllowance {
    /// Обычный режим: действует high-water mark audio buffer-а.
    Normal {
        /// Санитизированный high-water mark, мс.
        high_water_mark_ms: f64,
    },

    /// Стартовый video preroll голоден: временно разрешён больший потолок.
    VideoPrerollStarved {
        /// Санитизированный стартовый потолок, мс (не ниже high-water mark).
        cap_ms: f64,
    },

    /// Старт первого просмотра идёт внутри EOF drain: тот же стартовый потолок,
    /// чтобы уже считанный звук дошёл до output и хвост смог запуститься.
    StartupEofDrain {
        /// Санитизированный стартовый потолок, мс (не ниже high-water mark).
        cap_ms: f64,
    },
}

impl AudioReadAheadAllowance {
    /// Предел уровня audio buffer-а (мс), выше которого demux и audio decode ждут.
    #[must_use]
    pub(super) const fn limit_ms(self) -> f64 {
        match self {
            Self::Normal { high_water_mark_ms } => high_water_mark_ms,
            Self::VideoPrerollStarved { cap_ms } | Self::StartupEofDrain { cap_ms } => cap_ms,
        }
    }
}

/// Выбирает действующий предел audio read-ahead по состоянию session.
///
/// Решения о «голодном» preroll-е и о старте внутри EOF drain принадлежат session
/// (`video_preroll_is_starved`, `eof_drain_belongs_to_startup_playthrough`); здесь
/// только применяется policy из tick config.
#[must_use]
pub(super) fn audio_read_ahead_allowance(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> AudioReadAheadAllowance {
    let high_water_mark_ms =
        sanitize_audio_high_water_mark(tick_config.audio_buffer_high_water_mark_ms);
    let startup_cap_ms = || {
        sanitize_startup_read_ahead_cap_ms(
            tick_config.audio_startup_read_ahead_cap_ms,
            high_water_mark_ms,
        )
    };
    if session.video_preroll_is_starved() {
        AudioReadAheadAllowance::VideoPrerollStarved {
            cap_ms: startup_cap_ms(),
        }
    } else if session.eof_drain_belongs_to_startup_playthrough() {
        AudioReadAheadAllowance::StartupEofDrain {
            cap_ms: startup_cap_ms(),
        }
    } else {
        AudioReadAheadAllowance::Normal { high_water_mark_ms }
    }
}

/// Предел уровня audio buffer-а (мс) для demux admission и audio decode throttle.
#[must_use]
pub(super) fn audio_read_ahead_limit_ms(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> f64 {
    audio_read_ahead_allowance(session, tick_config).limit_ms()
}

/// Приводит стартовый потолок к безопасному диапазону.
///
/// Нефинитный/неположительный потолок отключает расширение (остаётся high-water);
/// слишком большой ограничивается ёмкостью audio output; потолок никогда не ниже
/// обычного high-water mark, иначе он бы сужал, а не расширял read-ahead.
#[must_use]
pub(super) fn sanitize_startup_read_ahead_cap_ms(cap_ms: f64, high_water_mark_ms: f64) -> f64 {
    if !cap_ms.is_finite() || cap_ms <= 0.0 {
        return high_water_mark_ms;
    }
    cap_ms
        .min(AUDIO_STARTUP_READ_AHEAD_SAFE_MAX_MS)
        .max(high_water_mark_ms)
}
