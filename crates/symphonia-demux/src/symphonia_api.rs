use std::path::Path;
use std::time::Duration;

use media_core::{TimeBase as MediaTimeBase, TrackTimestampUnits};
pub(crate) use symphonia::core::errors::Error as SymphoniaError;
pub(crate) use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader};
pub(crate) use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::error::DemuxError;

/// Owned Symphonia reader, который demuxer хранит между вызовами `next_packet`.
pub(crate) type FormatReaderBox<'source> = Box<dyn FormatReader + 'source>;

/// Создаёт probe hint из расширения, не раскрывая Symphonia API в demuxer-е.
pub(crate) fn hint_from_extension(extension_hint: &str) -> Hint {
    let mut hint = Hint::new();
    let extension = extension_hint.trim().trim_start_matches('.');

    if !extension.is_empty() {
        hint.with_extension(extension);
    }

    hint
}

/// Создаёт probe hint из file path для seekable local media.
pub(crate) fn hint_from_path(path: &Path) -> Hint {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(hint_from_extension)
        .unwrap_or_default()
}

/// Прячет Symphonia 0.6 `Probe::probe` и форму результата за локальной границей.
pub(crate) fn probe_format_reader<'source>(
    hint: &Hint,
    media_source_stream: MediaSourceStream<'source>,
) -> Result<FormatReaderBox<'source>, DemuxError> {
    symphonia::default::get_probe()
        .probe(
            hint,
            media_source_stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| DemuxError::UnsupportedFormat(error.to_string()))
}

/// Конвертирует Symphonia 0.6 `TimeBase` с `NonZero` полями в neutral media-core type.
pub(crate) fn media_time_base_from_symphonia(time_base: TimeBase) -> Option<MediaTimeBase> {
    MediaTimeBase::new(time_base.numer.get(), time_base.denom.get())
}

/// Конвертирует signed Symphonia timestamp в неотрицательную media timeline позицию.
pub(crate) fn symphonia_timestamp_to_duration(
    time_base: TimeBase,
    timestamp: Timestamp,
) -> Duration {
    media_time_base_from_symphonia(time_base)
        .map(|media_time_base| {
            media_time_base.track_units_to_duration(TrackTimestampUnits::new(timestamp.get()))
        })
        .unwrap_or_default()
}

/// Конвертирует unsigned Symphonia duration units в Rust `Duration`.
pub(crate) fn symphonia_duration_to_duration(
    time_base: TimeBase,
    duration: symphonia::core::units::Duration,
) -> Duration {
    media_time_base_from_symphonia(time_base)
        .map(|media_time_base| media_time_base.timestamp_to_duration(duration.get()))
        .unwrap_or_default()
}

/// Конвертирует Rust `Duration` в Symphonia 0.6 `Time` без доступа к приватным полям.
pub(crate) fn duration_to_symphonia_time(duration: Duration) -> Time {
    Time::try_from_nanos_u128(duration.as_nanos()).unwrap_or(Time::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use symphonia::core::units::{TimeBase, Timestamp};

    use super::{
        duration_to_symphonia_time, hint_from_extension, media_time_base_from_symphonia,
        symphonia_timestamp_to_duration,
    };

    #[test]
    fn extension_hint_trims_dot_prefix() {
        let hint = hint_from_extension(".webm");

        assert_eq!(
            format!("{hint:?}"),
            "Hint { extension: Some(\"webm\"), mime_type: None }"
        );
    }

    #[test]
    fn converts_symphonia_time_base_to_media_time_base() {
        let time_base = TimeBase::try_new(1, 1_000).expect("valid time base");

        let media_time_base =
            media_time_base_from_symphonia(time_base).expect("time base must convert");

        assert_eq!(media_time_base.numer, 1);
        assert_eq!(media_time_base.denom, 1_000);
    }

    #[test]
    fn converts_symphonia_timestamp_to_duration() {
        let time_base = TimeBase::try_new(1, 1_000).expect("valid time base");

        let duration = symphonia_timestamp_to_duration(time_base, Timestamp::new(2_750));

        assert_eq!(duration, Duration::from_millis(2_750));
    }

    #[test]
    fn negative_symphonia_timestamp_saturates_to_zero_duration() {
        let time_base = TimeBase::try_new(1, 1_000).expect("valid time base");

        let duration = symphonia_timestamp_to_duration(time_base, Timestamp::new(-25));

        assert_eq!(duration, Duration::ZERO);
    }

    #[test]
    fn converts_duration_to_symphonia_time() {
        let time = duration_to_symphonia_time(Duration::new(12, 250_000_000));

        assert_eq!(time.as_nanos(), 12_250_000_000);
        assert_eq!(time.as_secs_f64(), 12.25);
    }
}
