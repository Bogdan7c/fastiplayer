//! Ограниченная кольцевая история passthrough PCM для прайминга tempo processor-а.
//!
//! Модуль владеет сэмплами истории и PCM spec, под который они записаны. История
//! нужна только пока tempo processor отсутствует: свежий processor праймится ею,
//! чтобы phase vocoder не стартовал с пустого окна.
//!
//! Раньше история была `Vec<f32>`, а переполнение срезалось `drain(..n)` с начала —
//! то есть memmove всей истории (до сотен KB) на КАЖДОМ аудиопакете. Теперь это
//! `VecDeque`: срез старого хвоста только двигает «голову» кольца, без сдвига данных.

use std::collections::VecDeque;

/// Бюджет warmup истории passthrough PCM в миллисекундах.
///
/// Должен с запасом покрывать startup latency tempo backend-а (у текущего
/// Signalsmith preset_default это ~120-150 ms; бюджет держит headroom и для
/// более латентных бэкендов), оставаясь bounded по памяти
/// (600 ms 48k stereo f32 ≈ 230 KB).
const PASSTHROUGH_AUDIO_HISTORY_MAX_MS: u64 = 600;

/// Считает bounded сэмпловый бюджет warmup истории для decoded PCM spec.
fn passthrough_audio_history_max_samples(sample_rate: u32, channels: u32) -> usize {
    let frames = (u64::from(sample_rate) * PASSTHROUGH_AUDIO_HISTORY_MAX_MS).div_ceil(1000);
    usize::try_from(frames * u64::from(channels)).unwrap_or(usize::MAX)
}

/// Bounded кольцевая история последнего passthrough PCM одного `AudioOutputSpec`.
///
/// Инварианты: история всегда принадлежит одному spec; её длина после записи не
/// превышает бюджет (кроме пакета, который сам больше бюджета и режется целиком до
/// бюджета); хвост всегда выровнен по кадрам каналов при обрезке.
#[derive(Debug, Default)]
pub(super) struct PassthroughAudioHistory {
    /// Interleaved f32 сэмплы, старые спереди, новые сзади.
    samples: VecDeque<f32>,

    /// Полный decoded PCM spec, под который записана история.
    spec: Option<audio_core::AudioOutputSpec>,
}

impl PassthroughAudioHistory {
    /// Дописывает PCM в историю и срезает самый старый хвост сверх бюджета.
    ///
    /// Смена spec начинает историю заново: PCM другого формата смешивать нельзя.
    pub(super) fn record(&mut self, new_samples: &[f32], output_spec: audio_core::AudioOutputSpec) {
        let sample_rate = output_spec.sample_rate;
        let channels = output_spec.channels();
        if sample_rate == 0 || channels == 0 || new_samples.is_empty() {
            return;
        }

        if self.spec != Some(output_spec) {
            self.samples.clear();
            self.spec = Some(output_spec);
        }

        let max_samples = passthrough_audio_history_max_samples(sample_rate, channels);
        // Ёмкость резервируется один раз с запасом на пакет: после прогрева кольцо
        // больше не реаллоцируется (VecDeque::reserve — no-op, если места хватает).
        self.samples.reserve(
            max_samples
                .saturating_add(new_samples.len())
                .saturating_sub(self.samples.len()),
        );
        self.samples.extend(new_samples);

        let overflow = self.samples.len().saturating_sub(max_samples);
        if overflow > 0 {
            // Режем по границе кадра, чтобы история не начиналась посреди кадра.
            let frame_aligned_overflow = overflow.next_multiple_of(channels as usize);
            let drop_count = frame_aligned_overflow.min(self.samples.len());
            self.samples.drain(..drop_count);
        }
    }

    /// Забирает историю для прайминга; при несовпадении spec возвращает пустой `Vec`.
    ///
    /// История одноразовая: после take она пуста и без spec.
    pub(super) fn take_for_priming(
        &mut self,
        output_spec: audio_core::AudioOutputSpec,
    ) -> Vec<f32> {
        let history = std::mem::take(&mut self.samples);
        let matches_spec = self.spec == Some(output_spec);
        self.spec = None;
        if matches_spec {
            Vec::from(history)
        } else {
            Vec::new()
        }
    }

    /// Полный PCM spec доступной истории.
    pub(super) const fn spec(&self) -> Option<audio_core::AudioOutputSpec> {
        self.spec
    }

    /// Сбрасывает историю на seek/media boundary. Выделенная память остаётся кольцу.
    pub(super) fn clear(&mut self) {
        self.samples.clear();
        self.spec = None;
    }

    /// Ёмкость кольца в сэмплах (для тестов стабильности аллокации).
    #[cfg(test)]
    pub(super) fn capacity_for_tests(&self) -> usize {
        self.samples.capacity()
    }
}
