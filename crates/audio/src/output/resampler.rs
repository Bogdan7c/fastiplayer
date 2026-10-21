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

    /// Переиспользуемый scratch: carry-frame + текущий input chunk.
    ///
    /// Раньше собирался в новом `Vec` на каждый вызов; теперь ёмкость растёт до
    /// размера самого большого пакета и дальше не реаллоцируется.
    combined_samples: Vec<f32>,
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
            combined_samples: Vec::new(),
        }
    }

    /// Возвращает шаг source frames на один output frame.
    pub(super) fn source_frames_per_output_frame(&self) -> f64 {
        self.source_rate as f64 / self.output_rate as f64
    }

    /// Делает linear resample interleaved samples в `resampled_samples` без смешивания каналов.
    ///
    /// `resampled_samples` очищается и заполняется заново (ёмкость сохраняется),
    /// поэтому вызывающий код держит один буфер на весь output stream. При
    /// невалидных частотах или пустом input результат пуст, как и раньше.
    pub(super) fn resample_interleaved_into(
        &mut self,
        source_samples: &[f32],
        resampled_samples: &mut Vec<f32>,
    ) {
        resampled_samples.clear();
        if self.source_rate == 0 || self.output_rate == 0 {
            return;
        }

        let source_frame_count = source_samples.len() / self.channel_count;
        if source_frame_count == 0 {
            return;
        }

        let source_samples = &source_samples[..source_frame_count * self.channel_count];
        let carry_frame_count = usize::from(!self.previous_source_frame.is_empty());
        let combined_frame_count = source_frame_count + carry_frame_count;

        // Scratch переиспользуется: clear сохраняет ёмкость, extend не реаллоцирует после прогрева.
        self.combined_samples.clear();
        self.combined_samples
            .extend_from_slice(&self.previous_source_frame);
        self.combined_samples.extend_from_slice(source_samples);
        let combined_samples = &self.combined_samples;

        let mut source_frame_index =
            (self.next_source_frame_offset + carry_frame_count as f64).max(0.0);
        let source_frame_step = self.source_frames_per_output_frame();

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
    }

    /// Тестовая обёртка: старый контракт «вернуть новый `Vec`».
    #[cfg(test)]
    pub(super) fn resample_interleaved(&mut self, source_samples: &[f32]) -> Vec<f32> {
        let mut resampled_samples = Vec::new();
        self.resample_interleaved_into(source_samples, &mut resampled_samples);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Эталон старого алгоритма: два новых `Vec` на вызов, без переиспользования.
    struct ReferenceResampler {
        source_rate: u32,
        output_rate: u32,
        channel_count: usize,
        next_offset: f64,
        previous_frame: Vec<f32>,
    }

    impl ReferenceResampler {
        fn resample(&mut self, source: &[f32]) -> Vec<f32> {
            let frames = source.len() / self.channel_count;
            if frames == 0 {
                return Vec::new();
            }
            let source = &source[..frames * self.channel_count];
            let carry = usize::from(!self.previous_frame.is_empty());
            let total = frames + carry;
            let mut combined = Vec::with_capacity(total * self.channel_count);
            combined.extend_from_slice(&self.previous_frame);
            combined.extend_from_slice(source);
            let mut index = (self.next_offset + carry as f64).max(0.0);
            let step = self.source_rate as f64 / self.output_rate as f64;
            let mut out = Vec::new();
            while index.is_finite() && (index as usize) + 1 < total {
                let frame = index as usize;
                let fraction = index - frame as f64;
                for channel in 0..self.channel_count {
                    let a = combined[frame * self.channel_count + channel] as f64;
                    let b = combined[(frame + 1) * self.channel_count + channel] as f64;
                    out.push((a + fraction * (b - a)) as f32);
                }
                index += step;
            }
            self.next_offset = index - carry as f64 - frames as f64;
            self.previous_frame = source[(frames - 1) * self.channel_count..].to_vec();
            out
        }
    }

    #[test]
    fn reused_buffers_match_reference_bit_for_bit_and_do_not_reallocate() {
        let channel_count = 2;
        let mut resampler = LinearResampler::new(44_100, 48_000, channel_count);
        let mut reference = ReferenceResampler {
            source_rate: 44_100,
            output_rate: 48_000,
            channel_count,
            next_offset: 0.0,
            previous_frame: Vec::new(),
        };
        let mut output = Vec::new();

        // Одинаковые по размеру пакеты (как у реального декодера), разные данные.
        let packet_for = |seed: usize| -> Vec<f32> {
            (0..960 * channel_count)
                .map(|i| ((i * 31 + seed * 17) % 200) as f32 / 100.0 - 1.0)
                .collect()
        };

        // Прогрев: длина выхода колеблется на пару кадров из-за дробной позиции,
        // поэтому ёмкость выходит на плато не с первого пакета.
        for seed in 0..8 {
            resampler.resample_interleaved_into(&packet_for(seed), &mut output);
            assert_eq!(
                output,
                reference.resample(&packet_for(seed)),
                "прогрев {seed}"
            );
        }

        // После прогрева повторные вызовы того же размера не растят и не двигают буферы.
        let output_ptr = output.as_ptr();
        let output_capacity = output.capacity();
        let scratch_ptr = resampler.combined_samples.as_ptr();
        let scratch_capacity = resampler.combined_samples.capacity();

        for seed in 8..60 {
            resampler.resample_interleaved_into(&packet_for(seed), &mut output);
            assert_eq!(
                output,
                reference.resample(&packet_for(seed)),
                "пакет {seed}"
            );
            assert_eq!(
                output.as_ptr(),
                output_ptr,
                "output реаллоцирован, пакет {seed}"
            );
            assert_eq!(output.capacity(), output_capacity);
            assert_eq!(resampler.combined_samples.as_ptr(), scratch_ptr);
            assert_eq!(resampler.combined_samples.capacity(), scratch_capacity);
        }
    }

    #[test]
    fn empty_or_invalid_input_clears_output_and_keeps_state() {
        let mut resampler = LinearResampler::new(3, 4, 1);
        let mut output = vec![9.0; 4];
        resampler.resample_interleaved_into(&[], &mut output);
        assert!(output.is_empty());

        let mut invalid = LinearResampler::new(0, 4, 1);
        let mut output = vec![9.0; 4];
        invalid.resample_interleaved_into(&[1.0, 2.0], &mut output);
        assert!(output.is_empty());
    }
}
