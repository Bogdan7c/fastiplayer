//! Декодированный звук на выходе декодера и сборка interleaved-буфера.
//!
//! FFmpeg отдаёт AAC в формате float planar: каждый канал — отдельная
//! плоскость. Плеер ждёт interleaved f32 (L R L R …) в каноническом порядке
//! каналов. Перекладка — чистая функция, её закрепляют тесты в любой сборке.

use audio_core::AudioOutputSpec;

#[cfg(any(feature = "ffmpeg", test))]
use crate::channel_mapping::ChannelMapping;
#[cfg(any(feature = "ffmpeg", test))]
use crate::error::AudioFfmpegError;

/// PCM одного пакета: настоящий формат и interleaved-отсчёты.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAacPcm {
    /// Частота и раскладка именно этого буфера (у HE-AAC — выходная, удвоенная).
    pub output_spec: AudioOutputSpec,
    /// Interleaved f32 в каноническом порядке каналов `output_spec`.
    pub interleaved_samples: Vec<f32>,
}

// Склейка и перекладка нужны только FFI-слою; их тесты идут в любой сборке.
#[cfg(any(feature = "ffmpeg", test))]
impl DecodedAacPcm {
    /// Дописывает кадр того же формата в конец (один пакет — несколько кадров).
    pub(crate) fn append(&mut self, next_frame: DecodedAacPcm) -> Result<(), AudioFfmpegError> {
        if next_frame.output_spec != self.output_spec {
            return Err(AudioFfmpegError::MixedFramesInPacket);
        }
        self.interleaved_samples
            .extend_from_slice(&next_frame.interleaved_samples);
        Ok(())
    }
}

#[cfg(any(feature = "ffmpeg", test))]
/// Перекладывает плоскости FFmpeg в interleaved-буфер канонического порядка.
///
/// `planes[p]` — отсчёты плоскости FFmpeg с номером p; все плоскости одной длины.
pub(crate) fn interleave_planes(
    planes: &[&[f32]],
    mapping: &ChannelMapping,
) -> Result<Vec<f32>, AudioFfmpegError> {
    let frame_count = planes.first().map_or(0, |plane| plane.len());
    if planes.iter().any(|plane| plane.len() != frame_count) {
        return Err(AudioFfmpegError::UnsupportedChannelLayout {
            details: "planes of one frame have different lengths".to_owned(),
        });
    }
    if mapping
        .source_plane_for_lane
        .iter()
        .any(|source_plane| *source_plane >= planes.len())
    {
        return Err(AudioFfmpegError::UnsupportedChannelLayout {
            details: format!(
                "mapping refers to a plane beyond the {} planes of the frame",
                planes.len()
            ),
        });
    }

    // Для канонического канала k — итератор по отсчётам его плоскости FFmpeg.
    let mut lane_samples: Vec<std::slice::Iter<'_, f32>> = mapping
        .source_plane_for_lane
        .iter()
        .map(|source_plane| planes[*source_plane].iter())
        .collect();
    let mut interleaved_samples = Vec::with_capacity(frame_count * lane_samples.len());
    // Кадр за кадром: по одному отсчёту из каждого канала в каноническом порядке.
    for _ in 0..frame_count {
        for lane in &mut lane_samples {
            // Длины плоскостей проверены выше, поэтому отсчёт есть всегда.
            interleaved_samples.extend(lane.next().copied());
        }
    }
    Ok(interleaved_samples)
}

#[cfg(test)]
mod tests {
    use audio_core::{AudioChannelLayout, AudioChannelPosition};

    use super::*;

    fn stereo_mapping(source_plane_for_lane: Vec<usize>) -> ChannelMapping {
        ChannelMapping {
            layout: AudioChannelLayout::positioned(&[
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight,
            ])
            .expect("stereo"),
            source_plane_for_lane,
        }
    }

    #[test]
    fn planes_become_frames_in_canonical_lane_order() {
        let left = [0.1, 0.2, 0.3];
        let right = [-0.1, -0.2, -0.3];
        let interleaved =
            interleave_planes(&[&left, &right], &stereo_mapping(vec![0, 1])).expect("interleave");
        assert_eq!(interleaved, vec![0.1, -0.1, 0.2, -0.2, 0.3, -0.3]);
    }

    #[test]
    fn mapping_permutation_is_applied_per_frame() {
        // FFmpeg положил правый канал в плоскость 0: перестановка обязана это исправить.
        let right_first = [-1.0, -2.0];
        let left_second = [1.0, 2.0];
        let interleaved =
            interleave_planes(&[&right_first, &left_second], &stereo_mapping(vec![1, 0]))
                .expect("interleave");
        assert_eq!(interleaved, vec![1.0, -1.0, 2.0, -2.0]);
    }

    #[test]
    fn unequal_planes_and_missing_planes_are_refused() {
        let short = [0.0];
        let long = [0.0, 0.0];
        assert!(interleave_planes(&[&short, &long], &stereo_mapping(vec![0, 1])).is_err());
        assert!(interleave_planes(&[&short], &stereo_mapping(vec![0, 1])).is_err());
    }

    #[test]
    fn append_refuses_frames_of_another_format() {
        let stereo = AudioOutputSpec::new(
            44_100,
            AudioChannelLayout::from_channel_count(2).expect("stereo"),
        );
        let mut first = DecodedAacPcm {
            output_spec: stereo,
            interleaved_samples: vec![0.5, 0.5],
        };
        first
            .append(DecodedAacPcm {
                output_spec: stereo,
                interleaved_samples: vec![0.25, 0.25],
            })
            .expect("same format appends");
        assert_eq!(first.interleaved_samples, vec![0.5, 0.5, 0.25, 0.25]);

        let other_rate = DecodedAacPcm {
            output_spec: AudioOutputSpec::new(48_000, stereo.channel_layout),
            interleaved_samples: vec![0.0, 0.0],
        };
        assert_eq!(
            first.append(other_rate),
            Err(AudioFfmpegError::MixedFramesInPacket)
        );
    }
}
