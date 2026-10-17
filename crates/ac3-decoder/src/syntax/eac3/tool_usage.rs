//! Счётчики инструментов кодирования E-AC-3 в разобранном substream-е:
//! диагностика «что реально встретилось в потоке» (AHT, SPX, ECPL, TPNP).

use super::audblk::state::{CouplingMode, EnhancedCouplingConfig};
use super::audfrm::AudioFrame;

/// Сколько раз встретился каждый инструмент Annex E.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct EnhancedToolUsage {
    /// Разобранных аудиоблоков.
    pub audio_blocks: usize,
    /// Блоков со стандартным coupling.
    pub coupling_blocks: usize,
    /// Блоков с улучшенным coupling (ECPL, Annex E §3.5).
    pub enhanced_coupling_blocks: usize,
    /// Блоков со спектральным расширением (SPX, §3.6).
    pub spectral_extension_blocks: usize,
    /// Наборов экспонент (fbw, coupling, LFE), кодированных AHT (§3.4).
    pub aht_channels: usize,
    /// Каналов с параметрами подавления пред-шума (TPNP, §3.7).
    pub transient_pre_noise_channels: usize,
}

impl EnhancedToolUsage {
    /// Учитывает инструменты уровня кадра (`audfrm`).
    pub(crate) fn record_frame(&mut self, frame: &AudioFrame) {
        let aht = frame.aht;
        self.aht_channels += aht.channels.iter().filter(|&&on| on).count()
            + usize::from(aht.coupling)
            + usize::from(aht.lfe);
        self.transient_pre_noise_channels += frame
            .summary
            .transient_pre_noise
            .iter()
            .filter(|parameters| parameters.is_some())
            .count();
    }

    /// Учитывает инструменты одного аудиоблока.
    pub(crate) fn record_block(
        &mut self,
        coupling: Option<&EnhancedCouplingConfig>,
        spectral_extension: bool,
    ) {
        self.audio_blocks += 1;
        match coupling.map(|config| &config.mode) {
            Some(CouplingMode::Standard { .. }) => self.coupling_blocks += 1,
            Some(CouplingMode::Enhanced(_)) => self.enhanced_coupling_blocks += 1,
            None => {}
        }
        self.spectral_extension_blocks += usize::from(spectral_extension);
    }

    /// Складывает счётчики (сводка по нескольким substream-ам или кадрам).
    pub fn accumulate(&mut self, other: &Self) {
        self.audio_blocks += other.audio_blocks;
        self.coupling_blocks += other.coupling_blocks;
        self.enhanced_coupling_blocks += other.enhanced_coupling_blocks;
        self.spectral_extension_blocks += other.spectral_extension_blocks;
        self.aht_channels += other.aht_channels;
        self.transient_pre_noise_channels += other.transient_pre_noise_channels;
    }
}
