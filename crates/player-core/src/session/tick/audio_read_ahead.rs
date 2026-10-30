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
//!
//! Потолок ограничивает только декодированный звук в output. Чтение demux-а, пока
//! голодный preroll ждёт первый кадр, ограничивает не он, а запас отложенного сжатого
//! звука (сессия 03e моста, решение владельца). Если видео начинается позже звука
//! больше чем на потолок, первый видеопакет лежит в потоке за этим звуком: блокировка
//! по уровню output-а (звук до старта не играет и не убывает) означала вечный
//! `Buffering`. Инвариант: пока preroll ждёт первый кадр, уровень output-а не
//! запрещает чтение; его ограничивает только запас сжатого звука, а когда запас
//! исчерпан, session стартует со звуком первым (`session/video_preroll_wait.rs`).
//! Предел запаса мягкий: проверка идёт перед каждым read-ом, поэтому превышение не
//! больше read-ов одного tick-а (на практике — один packet).
//!
//! Тот же запас ограничивает звук, удержанный до своего PTS (начало отрезка позже базы
//! или разрыв посреди файла, `session/audio_gap.rs`): удержанный звук не декодируется,
//! output его не ограничивает, и без предела файл, где весь звук лежит раньше видео,
//! вылил бы его целиком в очередь. Исчерпан — demux ждёт; дедлока нет, потому что
//! время показа при удержании ведут monotonic-часы и звук войдёт в свой PTS сам
//! (а голодный старт откроют защёлкой «звук первым»). Seek preroll предел не трогает:
//! у него свои пределы, а блок demux-а мог бы не дать кадр цели.

use super::PlayerTickConfig;
use crate::pipeline::PendingAudioBacklog;
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

/// Хватает ли ещё запаса отложенного сжатого звука, пока preroll ждёт первый кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StartupAudioBacklogRoom {
    /// Preroll не голоден по видео: запас не действует, работает обычный предел.
    NotWaitingForVideo,

    /// Можно читать дальше и откладывать сжатый звук.
    Available,

    /// Запас исчерпан (по media-длительности или байтам), кадра всё нет.
    Exhausted(PendingAudioBacklog),
}

/// Оценивает запас отложенного сжатого звука для голодного preroll-а.
///
/// Решение «голоден ли preroll» принадлежит session (`video_preroll_is_starved`);
/// объём очереди — pipeline (`pending_audio_backlog`); здесь только сравнение с
/// пределами из tick config. Пустая очередь запас не исчерпывает никогда: сжатый звук
/// копится только когда output уже дошёл до потолка и декод остановлен. Поэтому
/// нулевой предел значит «без запаса»: старт со звуком первым сразу на потолке.
#[must_use]
pub(super) fn startup_audio_backlog_room(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> StartupAudioBacklogRoom {
    if !session.video_preroll_is_starved() {
        return StartupAudioBacklogRoom::NotWaitingForVideo;
    }
    let audio_backlog = session.pipeline.pending_audio_backlog();
    if audio_backlog_limit_reached(audio_backlog, tick_config) {
        StartupAudioBacklogRoom::Exhausted(audio_backlog)
    } else {
        StartupAudioBacklogRoom::Available
    }
}

/// Хватает ли запаса для звука, удержанного до своего PTS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HeldAudioBacklogRoom {
    /// Звук не удержан (или идёт seek preroll): предел не действует.
    NotHeld,

    /// Удержанный звук ещё помещается в запас.
    Available,

    /// Запас удержанного звука исчерпан: demux ждёт, пока звук войдёт в свой PTS.
    Exhausted(PendingAudioBacklog),
}

/// Оценивает запас сжатого звука, удержанного до своего PTS (те же пределы и тот же
/// владелец объёма очереди, что у голодного старта).
#[must_use]
pub(super) fn held_audio_backlog_room(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> HeldAudioBacklogRoom {
    if session.has_active_seek_commit() || !session.pending_audio_is_held_until_its_pts() {
        return HeldAudioBacklogRoom::NotHeld;
    }
    let audio_backlog = session.pipeline.pending_audio_backlog();
    if audio_backlog_limit_reached(audio_backlog, tick_config) {
        HeldAudioBacklogRoom::Exhausted(audio_backlog)
    } else {
        HeldAudioBacklogRoom::Available
    }
}

/// Достиг ли запас отложенного сжатого звука предела по длительности или байтам.
///
/// Пустая очередь предела не достигает никогда (нулевой предел — «без запаса»).
fn audio_backlog_limit_reached(
    audio_backlog: PendingAudioBacklog,
    tick_config: &PlayerTickConfig,
) -> bool {
    let span_exhausted =
        audio_backlog.media_span >= tick_config.startup_video_wait_max_audio_backlog;
    let bytes_exhausted =
        audio_backlog.encoded_bytes >= tick_config.startup_video_wait_max_audio_backlog_bytes;
    audio_backlog.packets > 0 && (span_exhausted || bytes_exhausted)
}

/// `true`, если уровень output-а не должен останавливать demux: preroll ждёт первый
/// кадр, а запас отложенного сжатого звука ещё есть.
///
/// Decode throttle при этом не меняется (`audio_read_ahead_limit_ms`): звук сверх
/// потолка не декодируется, а лежит в очереди сжатым, поэтому output не переполняется.
#[must_use]
pub(super) fn starved_preroll_reads_past_audio_limit(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    startup_audio_backlog_room(session, tick_config) == StartupAudioBacklogRoom::Available
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
