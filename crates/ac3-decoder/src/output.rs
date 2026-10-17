//! Публичный результат декодирования: PCM и описание каналов.
//!
//! Позиции каналов — реальные громкоговорители (A/52 Таблица 5.8 для AC-3,
//! Annex E Таблица E2.5 `chanmap` для зависимых substream-ов E-AC-3).
//! Downmix декодер не делает: каналы отдаются как есть, сводит приложение.

use crate::syntax::bsi_values::AudioCodingMode;

/// Позиция громкоговорителя канала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelPosition {
    /// L.
    Left,
    /// C.
    Center,
    /// R.
    Right,
    /// Ls (у AC-3 2/2 и 3/2 — SL).
    LeftSurround,
    /// Rs (у AC-3 2/2 и 3/2 — SR).
    RightSurround,
    /// Cs — тыловой центральный; так же отдаётся моно-surround S режимов 2/1 и 3/1.
    CenterSurround,
    /// Lc.
    LeftCenter,
    /// Rc.
    RightCenter,
    /// Lrs.
    LeftRearSurround,
    /// Rrs.
    RightRearSurround,
    /// Ts.
    TopSurround,
    /// Lsd.
    LeftSurroundDirect,
    /// Rsd.
    RightSurroundDirect,
    /// Lw.
    LeftWide,
    /// Rw.
    RightWide,
    /// Vhl.
    VerticalHeightLeft,
    /// Vhr.
    VerticalHeightRight,
    /// Vhc.
    VerticalHeightCenter,
    /// Lts.
    LeftTopSurround,
    /// Rts.
    RightTopSurround,
    /// LFE2.
    LowFrequencyEffects2,
    /// LFE.
    LowFrequencyEffects,
}

/// Позиции каналов AC-3 в порядке декодирования (Таблица 5.8) + LFE последним.
///
/// Режим 1+1 (две независимые моно-программы Ch1/Ch2) отдаётся как L/R:
/// так программы слышны обе; какую выбрать, решает приложение по
/// [`AudioCodingMode::DualMono`] в метаданных.
#[must_use]
pub fn ac3_channel_positions(mode: AudioCodingMode, lfe_on: bool) -> Vec<ChannelPosition> {
    use ChannelPosition::{
        Center, CenterSurround, Left, LeftSurround, LowFrequencyEffects, Right, RightSurround,
    };
    let mut positions = match mode {
        AudioCodingMode::DualMono | AudioCodingMode::Stereo => vec![Left, Right],
        AudioCodingMode::Mono => vec![Center],
        AudioCodingMode::ThreeFront => vec![Left, Center, Right],
        AudioCodingMode::StereoMonoSurround => vec![Left, Right, CenterSurround],
        AudioCodingMode::ThreeFrontMonoSurround => vec![Left, Center, Right, CenterSurround],
        AudioCodingMode::StereoStereoSurround => vec![Left, Right, LeftSurround, RightSurround],
        AudioCodingMode::ThreeFrontStereoSurround => {
            vec![Left, Center, Right, LeftSurround, RightSurround]
        }
    };
    if lfe_on {
        positions.push(LowFrequencyEffects);
    }
    positions
}

/// Декодированный access unit: планарный f32 PCM без ограничения амплитуды.
///
/// Масштаб: 1,0 — полная шкала (выход §7.9.4 шага 6 как есть). Значения
/// выше 1,0 возможны (ошибка кодирования поверх сигнала у 0 dBFS, §7.9.4) и
/// не обрезаются. Порядок каналов — порядок декодирования (Таблица 5.8,
/// LFE последним), смысл каждого канала — в [`Self::channels`]; приложение
/// раскладывает их по своим позициям. Планарный формат выбран, потому что
/// синтез пишет каналы независимо, а адаптер всё равно переупорядочивает.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    sample_rate_hz: u32,
    channels: Vec<ChannelPosition>,
    samples_per_channel: usize,
    /// Каналы подряд: `channels.len() × samples_per_channel` значений.
    planar_samples: Vec<f32>,
}

impl DecodedAudio {
    /// Собирает результат из планарных сэмплов (`channels.len() ×
    /// samples_per_channel` значений, каналы подряд).
    pub(crate) fn from_planar(
        sample_rate_hz: u32,
        channels: Vec<ChannelPosition>,
        samples_per_channel: usize,
        planar_samples: Vec<f32>,
    ) -> Self {
        debug_assert_eq!(planar_samples.len(), channels.len() * samples_per_channel);
        Self {
            sample_rate_hz,
            channels,
            samples_per_channel,
            planar_samples,
        }
    }

    /// Частота дискретизации.
    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Позиции каналов в порядке [`Self::channel_samples`].
    #[must_use]
    pub fn channels(&self) -> &[ChannelPosition] {
        &self.channels
    }

    /// Сэмплов на канал (256 × число аудиоблоков).
    #[must_use]
    pub const fn samples_per_channel(&self) -> usize {
        self.samples_per_channel
    }

    /// Все сэмплы планарно: канал 0 целиком, затем канал 1 и т. д.
    #[must_use]
    pub fn planar_samples(&self) -> &[f32] {
        &self.planar_samples
    }

    /// Сэмплы канала с индексом `channel`, если он есть.
    #[must_use]
    pub fn channel_samples(&self, channel: usize) -> Option<&[f32]> {
        let start = channel.checked_mul(self.samples_per_channel)?;
        self.planar_samples
            .get(start..start.checked_add(self.samples_per_channel)?)
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelPosition, ac3_channel_positions};
    use crate::syntax::bsi_values::AudioCodingMode;

    #[test]
    fn positions_follow_table_5_8_with_lfe_last() {
        for acmod in 0..8 {
            let mode = AudioCodingMode::from_code(acmod);
            for lfe_on in [false, true] {
                let positions = ac3_channel_positions(mode, lfe_on);
                assert_eq!(
                    positions.len(),
                    mode.full_bandwidth_channels() + usize::from(lfe_on)
                );
                assert_eq!(
                    positions.last() == Some(&ChannelPosition::LowFrequencyEffects),
                    lfe_on
                );
            }
        }
        assert_eq!(
            ac3_channel_positions(AudioCodingMode::ThreeFrontMonoSurround, false),
            [
                ChannelPosition::Left,
                ChannelPosition::Center,
                ChannelPosition::Right,
                ChannelPosition::CenterSurround
            ]
        );
    }
}
