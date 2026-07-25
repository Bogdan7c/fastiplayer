use std::time::Duration;

use super::audio_packet_window::DecodedPcmFrameRange;

/// Результат прежнего global/CUE расчёта в исходном decoded-frame domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecodedAudioPlaybackFrameRange {
    /// Нулевой sample rate/channel count сохраняет старое поведение без trim-а.
    PreserveOriginalSamples,

    /// Валидный output format допускает точный contiguous frame range.
    Frames(DecodedPcmFrameRange),
}

/// Вычисляет PCM frames внутри absolute playback bounds.
///
/// `media_clock_base` задаёт уже существующую нижнюю границу Accurate seek,
/// а `playback_end_exclusive` добавляет optional верхнюю границу neutral window.
/// Duration/floor arithmetic намеренно буквально сохранена.
pub(super) fn decoded_audio_playback_frame_range(
    sample_count: usize,
    packet_pts: Duration,
    media_clock_base: Duration,
    playback_end_exclusive: Option<Duration>,
    sample_rate: u32,
    channels: u32,
) -> DecodedAudioPlaybackFrameRange {
    if sample_rate == 0 || channels == 0 {
        return DecodedAudioPlaybackFrameRange::PreserveOriginalSamples;
    }

    let channel_count = channels as usize;
    let frame_count = sample_count / channel_count;
    if frame_count == 0 {
        return DecodedAudioPlaybackFrameRange::Frames(DecodedPcmFrameRange::full(0));
    }

    let first_frame = if packet_pts < media_clock_base {
        duration_to_audio_frames(media_clock_base.saturating_sub(packet_pts), sample_rate)
            .min(frame_count)
    } else {
        0
    };
    let end_frame = playback_end_exclusive.map_or(frame_count, |playback_end| {
        if packet_pts >= playback_end {
            0
        } else {
            duration_to_audio_frames(playback_end.saturating_sub(packet_pts), sample_rate)
                .min(frame_count)
        }
    });
    DecodedAudioPlaybackFrameRange::Frames(DecodedPcmFrameRange::ordered_or_empty(
        first_frame,
        end_frame,
    ))
}

/// Конвертирует duration в количество audio frames с округлением вниз.
fn duration_to_audio_frames(duration: Duration, sample_rate: u32) -> usize {
    let frames = duration.as_nanos().saturating_mul(u128::from(sample_rate)) / 1_000_000_000u128;

    frames.min(usize::MAX as u128) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Применяет новый range API так же, как старый trimming helper.
    fn slice_global_range(
        samples: &[f32],
        range: DecodedAudioPlaybackFrameRange,
        channels: u32,
    ) -> &[f32] {
        match range {
            DecodedAudioPlaybackFrameRange::PreserveOriginalSamples => samples,
            DecodedAudioPlaybackFrameRange::Frames(frames) => frames
                .slice_interleaved(samples, channels)
                .expect("global range должен принадлежать decoded block"),
        }
    }

    #[test]
    fn trims_start_and_exclusive_end_in_one_pcm_frame_domain() {
        let samples = (0..20).map(|sample| sample as f32).collect::<Vec<_>>();

        let range = decoded_audio_playback_frame_range(
            samples.len(),
            Duration::from_secs(9),
            Duration::from_millis(9_500),
            Some(Duration::from_millis(10_500)),
            4,
            2,
        );
        let trimmed = slice_global_range(&samples, range, 2);

        assert_eq!(trimmed, &[4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]);
    }

    #[test]
    fn packet_at_or_after_exclusive_end_produces_no_pcm() {
        let samples = [1.0, 2.0, 3.0, 4.0];

        let range = decoded_audio_playback_frame_range(
            samples.len(),
            Duration::from_secs(25),
            Duration::from_secs(10),
            Some(Duration::from_secs(25)),
            48_000,
            2,
        );
        let trimmed = slice_global_range(&samples, range, 2);

        assert!(trimmed.is_empty());
    }
}
