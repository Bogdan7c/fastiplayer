//! Сдвиг шкалы дорожки между временем контейнера (как его отдаёт Symphonia) и медиа-временем.
//!
//! Зачем: Symphonia 0.6 договорилась, что отрицательный pts означает «прогрев кодера, который
//! декодер выбросит» (Matroska `CodecDelay`, MP4 edit list у нас уже так и работают). Но upstream
//! `symphonia-format-ogg` переводит Ogg-гранулу в timestamp без вычитания `pre_skip` из OpusHead,
//! хотя RFC 7845 §4.2 определяет медиа-позицию отсчёта как `granule − pre_skip`. Итог — первый
//! пакет Ogg Opus получает pts 0 вместо −6.5 мс, и весь звук Ogg Opus опаздывает на `pre_skip`.
//!
//! Модуль владеет только этим знанием: какая дорожка какого контейнера нуждается в сдвиге и как
//! переводить timestamp-ы в обе стороны (packet/seek-результат → медиа, seek-цель → контейнер).
//! Декодер прогрев сам НЕ выбрасывает: как и для Matroska, прогрев живёт на отрицательном pts,
//! поэтому двойной коррекции нет. Matroska/WebM сюда не попадают: их патч уже вычитает
//! `CodecDelay` из pts, хотя в CodecPrivate лежит тот же OpusHead с тем же `pre_skip`.
//!
//! Если upstream Ogg начнёт вычитать `pre_skip` сам, этот сдвиг нужно убрать: тест
//! `tests/ogg_opus_pre_skip.rs` ждёт ровно −312 у первого пакета и поймает двойной сдвиг (−624).

use std::time::Duration;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::formats::well_known::FORMAT_ID_OGG;
use symphonia::core::formats::{FormatId, Track};
use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase, Timestamp};
use tracing::warn;

use crate::symphonia_api::symphonia_duration_to_duration;

/// Opus всегда декодируется в 48 кГц, и `pre_skip` в OpusHead задан в отсчётах этой частоты.
const OPUS_PRE_SKIP_SAMPLE_RATE: u32 = 48_000;

/// Сигнатура identification header-а Ogg Opus (RFC 7845 §5.1).
const OPUS_HEAD_MAGIC: &[u8; 8] = b"OpusHead";

/// Смещение 16-битного little-endian `pre_skip` внутри OpusHead.
const OPUS_HEAD_PRE_SKIP_OFFSET: usize = 10;

/// Насколько timestamp контейнера опережает медиа-время: `медиа = контейнер − leading_ticks`.
///
/// Тики — в time base самой дорожки. Нулевой сдвиг (`NONE`) — обычный случай для всех
/// контейнеров, которые уже отдают медиа-время.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ContainerTimestampShift {
    leading_ticks: u64,
}

impl ContainerTimestampShift {
    /// Контейнер уже отдаёт медиа-время: переводить нечего.
    pub(crate) const NONE: Self = Self { leading_ticks: 0 };

    /// Сдвиг на заданное число тиков — только для unit-тестов соседних mapper-ов.
    #[cfg(test)]
    pub(crate) const fn from_leading_ticks(leading_ticks: u64) -> Self {
        Self { leading_ticks }
    }

    /// Определяет сдвиг для дорожки конкретного контейнера.
    ///
    /// Сейчас сдвиг нужен только Ogg Opus. Остальные контейнеры и кодеки возвращают `NONE`.
    pub(crate) fn for_container_track(container_format: FormatId, track: &Track) -> Self {
        if container_format != FORMAT_ID_OGG {
            return Self::NONE;
        }
        let Some(CodecParameters::Audio(audio_params)) = track.codec_params.as_ref() else {
            return Self::NONE;
        };
        if audio_params.codec != CODEC_ID_OPUS {
            return Self::NONE;
        }
        ogg_opus_pre_skip_shift(
            track.id,
            audio_params.extra_data.as_deref(),
            track.time_base,
        )
    }

    /// Container timestamp packet-а или seek-результата → медиа-время той же дорожки.
    pub(crate) fn container_to_media(self, container_timestamp: Timestamp) -> Timestamp {
        // `checked_sub` переполняется только у самого края i64; такой timestamp уже бессмыслен,
        // поэтому прижимаем к минимуму вместо паники.
        i64::try_from(self.leading_ticks)
            .ok()
            .and_then(|leading_ticks| container_timestamp.get().checked_sub(leading_ticks))
            .map_or(Timestamp::new(i64::MIN), Timestamp::new)
    }

    /// Медиа-время seek-цели → время контейнера, в котором ищет reader.
    ///
    /// Без этого Ogg reader искал бы гранулу «цель», то есть медиа-время на `pre_skip` раньше.
    pub(crate) fn media_seek_time_to_container(
        self,
        media_time: Duration,
        time_base: Option<TimeBase>,
    ) -> Duration {
        let Some(time_base) = time_base.filter(|_| self.leading_ticks > 0) else {
            return media_time;
        };
        let leading_duration =
            symphonia_duration_to_duration(time_base, SymphoniaDuration::new(self.leading_ticks));
        media_time.saturating_add(leading_duration)
    }
}

/// Сдвиг Ogg Opus = `pre_skip` из OpusHead (RFC 7845 §4.2: позиция PCM = гранула − pre_skip).
///
/// OpusHead — авторитетный источник `pre_skip`: Symphonia `Track::delay` upstream может
/// переписать при осмотре первой страницы, а OpusHead — нет.
fn ogg_opus_pre_skip_shift(
    track_id: u32,
    opus_head: Option<&[u8]>,
    time_base: Option<TimeBase>,
) -> ContainerTimestampShift {
    let Some(pre_skip_samples) = opus_head.and_then(opus_head_pre_skip_samples) else {
        warn!(
            track_id,
            "Ogg Opus track has no readable OpusHead pre_skip; timestamps stay unshifted"
        );
        return ContainerTimestampShift::NONE;
    };
    // Ogg Opus mapper ставит time base 1/48000, тогда тики = отсчёты. Иную шкалу не угадываем:
    // перевод отсчётов в чужие тики мог бы дать дробный сдвиг.
    let expected_time_base = TimeBase::try_new(1, OPUS_PRE_SKIP_SAMPLE_RATE);
    if time_base != expected_time_base {
        warn!(
            track_id,
            ?time_base,
            pre_skip_samples,
            "Ogg Opus track time base is not 1/48000; pre_skip is not applied to timestamps"
        );
        return ContainerTimestampShift::NONE;
    }
    ContainerTimestampShift {
        leading_ticks: u64::from(pre_skip_samples),
    }
}

/// Читает `pre_skip` из OpusHead; `None`, если это не OpusHead или он обрезан.
fn opus_head_pre_skip_samples(opus_head: &[u8]) -> Option<u16> {
    if !opus_head.starts_with(OPUS_HEAD_MAGIC) {
        return None;
    }
    let pre_skip_bytes: [u8; 2] = opus_head
        .get(OPUS_HEAD_PRE_SKIP_OFFSET..OPUS_HEAD_PRE_SKIP_OFFSET + 2)?
        .try_into()
        .ok()?;
    Some(u16::from_le_bytes(pre_skip_bytes))
}

#[cfg(test)]
#[path = "container_timestamp_shift/tests.rs"]
mod tests;
