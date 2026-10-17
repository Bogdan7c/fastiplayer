//! Overlap-add и хвосты каналов между блоками (A/52:2012 §7.9.4, шаг 6).
//!
//! `pcm[n] = 2·(x[n] + delay[n])`, `delay[n] = x[N/2 + n]` для n < 256:
//! первая половина взвешенного блока складывается со второй половиной
//! прошлого блока того же канала; множитель 2 снимает запас по уровню,
//! заложенный кодером. Спецификация требует насыщения для целочисленной
//! арифметики; здесь f32 и ограничения амплитуды нет намеренно (выход выше
//! 0 dBFS сохраняется, решение — у потребителя).
//!
//! Владелец хвостов — [`OverlapBank`]. Хвост принадлежит каналу раскладки
//! (частота + позиции каналов), поэтому при смене раскладки банк начинается
//! заново с нулевых хвостов: продолжать overlap чужого канала бессмысленно,
//! а смена раскладки — разрыв потока. Первый кадр после создания или
//! очистки складывается с нулями (это «разгон» TDAC: первые 256 отсчётов
//! содержат только нарастающую половину окна первого блока).

use super::transform::imdct::{TRANSFORM_LENGTH, WindowedBlock};
use crate::output::ChannelPosition;
use crate::syntax::syncframe::SAMPLES_PER_AUDIO_BLOCK;

/// Длина хвоста = N/2 = новых отсчётов в блоке.
const DELAY_LENGTH: usize = TRANSFORM_LENGTH / 2;

/// Множитель шага 6 («undoes headroom scaling performed in the encoder»).
const OVERLAP_OUTPUT_SCALE: f32 = 2.0;

/// Хвост одного канала.
#[derive(Debug, Clone)]
struct ChannelDelay {
    samples: [f32; DELAY_LENGTH],
}

impl ChannelDelay {
    const fn silent() -> Self {
        Self {
            samples: [0.0; DELAY_LENGTH],
        }
    }

    /// Шаг 6: пишет 256 отсчётов PCM и запоминает новую вторую половину.
    fn overlap_add(&mut self, windowed: &WindowedBlock, pcm: &mut [f32]) {
        let (first_half, second_half) = windowed.split_at(DELAY_LENGTH);
        for ((output, delay), (&current, &next)) in pcm
            .iter_mut()
            .zip(self.samples.iter_mut())
            .zip(first_half.iter().zip(second_half))
        {
            *output = OVERLAP_OUTPUT_SCALE * (current + *delay);
            *delay = next;
        }
    }
}

/// Раскладка, к которой относятся хвосты.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverlapLayout {
    sample_rate_hz: u32,
    positions: Vec<ChannelPosition>,
}

/// Хвосты всех каналов текущей раскладки.
#[derive(Debug, Clone, Default)]
pub(crate) struct OverlapBank {
    layout: Option<OverlapLayout>,
    delays: Vec<ChannelDelay>,
}

impl OverlapBank {
    /// Готовит хвосты для кадра с этой раскладкой; при смене раскладки (или
    /// после [`Self::discard`]) хвосты нулевые.
    pub(crate) fn begin_frame(&mut self, sample_rate_hz: u32, positions: &[ChannelPosition]) {
        let unchanged = self.layout.as_ref().is_some_and(|layout| {
            layout.sample_rate_hz == sample_rate_hz && layout.positions == positions
        });
        if !unchanged {
            self.layout = Some(OverlapLayout {
                sample_rate_hz,
                positions: positions.to_vec(),
            });
            self.delays = vec![ChannelDelay::silent(); positions.len()];
        }
    }

    /// Overlap-add блока канала `channel` (индекс в раскладке кадра) в
    /// `pcm` (256 отсчётов). Канал вне раскладки — ошибка программы; в
    /// release выход тогда не пишется.
    pub(crate) fn overlap_add(
        &mut self,
        channel: usize,
        windowed: &WindowedBlock,
        pcm: &mut [f32],
    ) {
        debug_assert_eq!(pcm.len(), SAMPLES_PER_AUDIO_BLOCK);
        debug_assert!(channel < self.delays.len());
        if let Some(delay) = self.delays.get_mut(channel) {
            delay.overlap_add(windowed, pcm);
        }
    }

    /// Забывает хвосты: следующий кадр начнётся как первый (разрыв потока —
    /// ошибка кадра, `reset`).
    pub(crate) fn discard(&mut self) {
        self.layout = None;
        self.delays.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{OverlapBank, TRANSFORM_LENGTH};
    use crate::output::ChannelPosition::{Center, Left, Right};

    fn block(first: f32, second: f32) -> [f32; TRANSFORM_LENGTH] {
        std::array::from_fn(|index| {
            if index < TRANSFORM_LENGTH / 2 {
                first
            } else {
                second
            }
        })
    }

    #[test]
    fn overlap_adds_previous_tail_and_doubles() {
        let mut bank = OverlapBank::default();
        bank.begin_frame(48_000, &[Left, Right]);
        let mut pcm = [0.0; 256];
        // Первый блок: хвост нулевой → pcm = 2·x[0..256].
        bank.overlap_add(0, &block(0.25, 0.5), &mut pcm);
        assert!(pcm.iter().all(|&sample| sample == 0.5));
        // Второй: 2·(0,125 + 0,5).
        bank.overlap_add(0, &block(0.125, 0.0), &mut pcm);
        assert!(pcm.iter().all(|&sample| sample == 1.25));
        // Хвост канала 1 не тронут каналом 0.
        bank.overlap_add(1, &block(0.0, 0.0), &mut pcm);
        assert!(pcm.iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn layout_change_and_discard_start_from_silent_tails() {
        let mut bank = OverlapBank::default();
        let mut pcm = [0.0; 256];
        bank.begin_frame(48_000, &[Left, Right]);
        bank.overlap_add(0, &block(0.0, 1.0), &mut pcm);
        // Та же раскладка — хвост сохраняется.
        bank.begin_frame(48_000, &[Left, Right]);
        bank.overlap_add(0, &block(0.0, 0.0), &mut pcm);
        assert_eq!(pcm[0], 2.0);
        // Смена позиций (и, отдельно, частоты) — нулевые хвосты.
        bank.overlap_add(0, &block(0.0, 1.0), &mut pcm);
        bank.begin_frame(48_000, &[Left, Center, Right]);
        bank.overlap_add(0, &block(0.0, 0.0), &mut pcm);
        assert_eq!(pcm[0], 0.0);
        bank.overlap_add(0, &block(0.0, 1.0), &mut pcm);
        bank.begin_frame(44_100, &[Left, Center, Right]);
        bank.overlap_add(0, &block(0.0, 0.0), &mut pcm);
        assert_eq!(pcm[0], 0.0);
        bank.overlap_add(0, &block(0.0, 1.0), &mut pcm);
        bank.discard();
        bank.begin_frame(44_100, &[Left, Center, Right]);
        bank.overlap_add(0, &block(0.0, 0.0), &mut pcm);
        assert_eq!(pcm[0], 0.0);
    }

    #[test]
    fn output_is_not_clamped_above_full_scale() {
        let mut bank = OverlapBank::default();
        bank.begin_frame(48_000, &[Left]);
        let mut pcm = [0.0; 256];
        bank.overlap_add(0, &block(0.75, 0.75), &mut pcm);
        bank.overlap_add(0, &block(0.75, 0.0), &mut pcm);
        assert!(pcm.iter().all(|&sample| sample == 3.0));
    }
}
