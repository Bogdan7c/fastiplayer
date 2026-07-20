use std::time::Duration;

/// Возвращает только PCM frames внутри absolute playback bounds.
///
/// `media_clock_base` задаёт уже существующую нижнюю границу Accurate seek,
/// а `playback_end_exclusive` добавляет optional верхнюю границу neutral window.
/// Обрезка выполняется до tempo/output path, поэтому соседний фрагмент не
/// просачивается в DAC и EOF drain ждёт только разрешённый audio tail.
pub(super) fn trim_decoded_audio_to_playback_bounds(
    samples: &[f32],
    packet_pts: Duration,
    media_clock_base: Duration,
    playback_end_exclusive: Option<Duration>,
    sample_rate: u32,
    channels: u32,
) -> &[f32] {
    if sample_rate == 0 || channels == 0 {
        return samples;
    }

    let channel_count = channels as usize;
    let frame_count = samples.len() / channel_count;
    if frame_count == 0 {
        return &[];
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
    if first_frame >= end_frame {
        return &[];
    }

    let first_sample = first_frame.saturating_mul(channel_count);
    let end_sample = end_frame.saturating_mul(channel_count);
    &samples[first_sample..end_sample]
}

/// Конвертирует duration в количество audio frames с округлением вниз.
fn duration_to_audio_frames(duration: Duration, sample_rate: u32) -> usize {
    let frames = duration.as_nanos().saturating_mul(u128::from(sample_rate)) / 1_000_000_000u128;

    frames.min(usize::MAX as u128) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_start_and_exclusive_end_in_one_pcm_frame_domain() {
        let samples = (0..20).map(|sample| sample as f32).collect::<Vec<_>>();

        let trimmed = trim_decoded_audio_to_playback_bounds(
            &samples,
            Duration::from_secs(9),
            Duration::from_millis(9_500),
            Some(Duration::from_millis(10_500)),
            4,
            2,
        );

        assert_eq!(trimmed, &[4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]);
    }

    #[test]
    fn packet_at_or_after_exclusive_end_produces_no_pcm() {
        let samples = [1.0, 2.0, 3.0, 4.0];

        let trimmed = trim_decoded_audio_to_playback_bounds(
            &samples,
            Duration::from_secs(25),
            Duration::from_secs(10),
            Some(Duration::from_secs(25)),
            48_000,
            2,
        );

        assert!(trimmed.is_empty());
    }
}
