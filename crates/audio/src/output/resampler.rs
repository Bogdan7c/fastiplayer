//! Stateful linear resampling между decoded packet-ами.
//!
//! Модуль владеет только fractional/carry state и не знает CPAL stream,
//! ring buffer, clock или device fallback policy.

/// Состояние linear resampling между decoded audio packets.
///
/// Opus отдаёт audio chunks пакетами, а output device может работать не на 48 kHz.
/// Чтобы на границе packets не было слышимого скачка, ресемплер хранит последний
/// source frame предыдущего packet-а и продолжает fractional позицию на следующем.
pub(super) struct LinearResampler {
    /// Sample rate декодированного audio.
    source_rate: u32,

    /// Sample rate output stream.
    output_rate: u32,

    /// Количество interleaved каналов в stream layout.
    channel_count: usize,

    /// Следующая source-позиция относительно начала нового input chunk.
    next_source_frame_offset: f64,

    /// Последний frame предыдущего input chunk для интерполяции через boundary.
    previous_source_frame: Vec<f32>,
}

impl LinearResampler {
    /// Создаёт linear resampler с явным соотношением частот.
    pub(super) fn new(source_rate: u32, output_rate: u32, channel_count: usize) -> Self {
        Self {
            source_rate,
            output_rate,
            channel_count: channel_count.max(1),
            next_source_frame_offset: 0.0,
            previous_source_frame: Vec::new(),
        }
    }

    /// Возвращает шаг source frames на один output frame.
    pub(super) fn source_frames_per_output_frame(&self) -> f64 {
        self.source_rate as f64 / self.output_rate as f64
    }

    /// Делает linear resample interleaved samples без смешивания каналов.
    pub(super) fn resample_interleaved(&mut self, source_samples: &[f32]) -> Vec<f32> {
        if self.source_rate == 0 || self.output_rate == 0 {
            return Vec::new();
        }

        let source_frame_count = source_samples.len() / self.channel_count;
        if source_frame_count == 0 {
            return Vec::new();
        }

        let source_samples = &source_samples[..source_frame_count * self.channel_count];
        let carry_frame_count = usize::from(!self.previous_source_frame.is_empty());
        let combined_frame_count = source_frame_count + carry_frame_count;
        let mut combined_samples = Vec::with_capacity(combined_frame_count * self.channel_count);

        combined_samples.extend_from_slice(&self.previous_source_frame);
        combined_samples.extend_from_slice(source_samples);

        let mut source_frame_index =
            (self.next_source_frame_offset + carry_frame_count as f64).max(0.0);
        let source_frame_step = self.source_frames_per_output_frame();
        let mut resampled_samples = Vec::new();

        while source_frame_index.is_finite()
            && (source_frame_index as usize) + 1 < combined_frame_count
        {
            let frame_index = source_frame_index as usize;
            let frame_fraction = source_frame_index - frame_index as f64;

            for channel_index in 0..self.channel_count {
                let current_sample =
                    combined_samples[frame_index * self.channel_count + channel_index] as f64;
                let next_sample =
                    combined_samples[(frame_index + 1) * self.channel_count + channel_index] as f64;
                let interpolated_sample =
                    current_sample + frame_fraction * (next_sample - current_sample);
                resampled_samples.push(interpolated_sample as f32);
            }

            source_frame_index += source_frame_step;
        }

        self.next_source_frame_offset =
            source_frame_index - carry_frame_count as f64 - source_frame_count as f64;
        self.remember_last_source_frame(source_samples, source_frame_count);

        resampled_samples
    }

    /// Запоминает последний complete frame текущего chunk для следующего boundary.
    pub(super) fn remember_last_source_frame(
        &mut self,
        source_samples: &[f32],
        source_frame_count: usize,
    ) {
        let last_frame_start = (source_frame_count - 1) * self.channel_count;
        let last_frame_end = last_frame_start + self.channel_count;

        self.previous_source_frame.clear();
        self.previous_source_frame
            .extend_from_slice(&source_samples[last_frame_start..last_frame_end]);
    }

    /// Сбрасывает carry state, чтобы после seek не смешивать старый и новый audio chunks.
    pub(super) fn reset(&mut self) {
        self.next_source_frame_offset = 0.0;
        self.previous_source_frame.clear();
    }
}
