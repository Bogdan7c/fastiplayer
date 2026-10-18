//! Чистая арифметика времени WebVTT-сегментов HLS: разворот 33-битного счётчика MPEG-TS
//! и перевод «время реплики -> время плеера». Без сети и без состояния.
//!
//! Формулы (RFC 8216 §3.5 и контракт `HlsTimestampOrigins`):
//! - `media = MPEGTS / 90000 + (cue - LOCAL)`: время реплики на СЫРОЙ шкале контейнера видео;
//! - `player = media - media_origin + timeline_start`: ровно то, что делает демуксер видео.

use std::time::Duration;

use subtitle_formats::MpegTsTimestamp90kHz;

/// Микросекунд в секунде (для i128-арифметики без потери точности).
const MICROS_PER_SECOND: i128 = 1_000_000;

/// Пара «точка отсчёта эпохи» в микросекундах (знаковых, чтобы считать без паник).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OriginMicros {
    /// Сырое время контейнера, принятое демуксером за ноль эпохи.
    pub(super) media_origin: i64,
    /// Начало эпохи на шкале плеера.
    pub(super) timeline_start: i64,
}

/// Длительность в микросекундах; `None`, если не помещается в i64.
pub(super) fn duration_micros(duration: Duration) -> Option<i64> {
    i64::try_from(duration.as_micros()).ok()
}

/// Разворачивает 33-битную метку MPEG-TS к значению, ближайшему к `reference`.
///
/// Счётчик 90 кГц оборачивается каждые 2^33 тиков (около 26,5 часа). Демуксер видео мог
/// видеть метки до оборота, а VTT - после (или наоборот), поэтому из всех кандидатов
/// `ticks + k * 2^33` берётся тот, что ближе к точке отсчёта эпохи. Результат может быть
/// отрицательным (метка чуть до оборота, точка отсчёта чуть после) - это нормально.
pub(super) fn unwrap_mpegts_near(mpegts: MpegTsTimestamp90kHz, reference: Duration) -> Option<i64> {
    let ticks_per_second = i128::from(MpegTsTimestamp90kHz::TICKS_PER_SECOND);
    let modulus = i128::from(MpegTsTimestamp90kHz::MODULUS);
    let reference_ticks =
        i128::try_from(reference.as_micros()).ok()? * ticks_per_second / MICROS_PER_SECOND;
    let base_ticks = i128::from(mpegts.ticks());
    // Ближайшее целое число оборотов: округление `(diff / modulus)` к ближайшему.
    let wraps = (reference_ticks - base_ticks + modulus / 2).div_euclid(modulus);
    let unwrapped_ticks = base_ticks + wraps * modulus;
    i64::try_from(unwrapped_ticks * MICROS_PER_SECOND / ticks_per_second).ok()
}

/// Сырое время контейнера -> время плеера по паре эпохи.
pub(super) fn media_to_player(media_micros: i64, origin: OriginMicros) -> Option<i64> {
    media_micros
        .checked_sub(origin.media_origin)?
        .checked_add(origin.timeline_start)
}

/// Сдвиг, который переводит время реплики (в системе VTT-файла) в время плеера.
///
/// `anchor_media` - сырое время контейнера, соответствующее `local` в файле.
pub(super) fn cue_to_player_shift(
    anchor_media_micros: i64,
    local_micros: i64,
    origin: OriginMicros,
) -> Option<i64> {
    media_to_player(anchor_media_micros, origin)?.checked_sub(local_micros)
}

/// Сколько микросекунд добавить к времени метки, чтобы развернуть оборот 33-битного счётчика
/// к значению, ближайшему к `reference` (кратно периоду оборота, обычно ноль).
///
/// Нужна live-сегментам без `X-TIMESTAMP-MAP`: по RFC 8216 §3.5 отсутствие карты равно
/// `MPEGTS:0 <-> LOCAL:0`, то есть время реплики и есть время контейнера - но записанное
/// счётчиком, который мог обернуться.
pub(super) fn wrap_shift_near(sample_micros: i64, reference: Duration) -> Option<i64> {
    let period_micros = i128::from(MpegTsTimestamp90kHz::MODULUS) * MICROS_PER_SECOND
        / i128::from(MpegTsTimestamp90kHz::TICKS_PER_SECOND);
    let reference_micros = i128::try_from(reference.as_micros()).ok()?;
    let wraps = (reference_micros - i128::from(sample_micros) + period_micros / 2)
        .div_euclid(period_micros);
    i64::try_from(wraps * period_micros).ok()
}
