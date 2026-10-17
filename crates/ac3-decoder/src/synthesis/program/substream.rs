//! Вид синтеза на один разобранный substream программы: что нужно для PCM,
//! без знания, AC-3 это ядро или кадр E-AC-3.
//!
//! Синтаксис (`syntax::eac3::access_unit`) отдаёт заголовки двух видов;
//! синтезу нужны только раскладка кодированных каналов, коэффициенты блоков,
//! слова `compr` (§7.7.2, Annex E §3.8.5) и параметры TPNP (Annex E §3.7).

use crate::output::ChannelPosition;
use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::syntax::audblk::output::DecodedAudioBlock;
use crate::syntax::bsi_values::AudioCodingMode;
use crate::syntax::eac3::access_unit::{ParsedSubstream, SubstreamHeader};
use crate::syntax::eac3::audfrm::TransientPreNoise;
use crate::syntax::syncframe::{AUDIO_BLOCKS_PER_SYNCFRAME, SyncframeInfo};

/// Параметры TPNP по fbw каналам substream-а (порядок кодирования).
pub(crate) type TransientPreNoiseParameters =
    [Option<TransientPreNoise>; MAX_FULL_BANDWIDTH_CHANNELS];

/// Слова «heavy compression» кадра (`compr`, `compr2`; §5.4.2.10/§5.4.2.18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct CompressionWords {
    /// `compr` программы Ch1 (и единственной программы вне 1+1).
    pub(crate) first_program: Option<u8>,
    /// `compr2` программы Ch2 режима 1+1.
    pub(crate) second_program: Option<u8>,
}

/// Один substream программы глазами синтеза.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SubstreamFrame<'a> {
    /// `acmod`: нужен, чтобы различить Ch2 режима 1+1 (своё слово DRC).
    pub(crate) coding_mode: AudioCodingMode,
    /// Позиции кодированных каналов: fbw по порядку потока, затем LFE.
    pub(crate) positions: &'a [ChannelPosition],
    /// Коэффициенты блоков кадра (ровно `audio_blocks` штук).
    pub(crate) blocks: &'a [DecodedAudioBlock],
    /// Слова `compr` кадра.
    pub(crate) compression: CompressionWords,
    /// TPNP по fbw каналам (у AC-3 инструмента нет — все `None`).
    pub(crate) transient_pre_noise: TransientPreNoiseParameters,
}

impl<'a> SubstreamFrame<'a> {
    /// Вид на substream, разобранный парсером access unit-а.
    pub(crate) fn from_parsed(substream: &ParsedSubstream<'a>) -> Self {
        match substream.header {
            SubstreamHeader::Ac3(info) => {
                Self::from_ac3(info, substream.positions, &substream.blocks[..])
            }
            SubstreamHeader::Enhanced(info) => Self {
                coding_mode: info.bsi.audio_coding_mode,
                positions: substream.positions,
                blocks: substream
                    .blocks
                    .get(..info.audio_blocks)
                    .unwrap_or(&substream.blocks[..]),
                compression: CompressionWords {
                    first_program: info.bsi.program.compression_gain_word,
                    second_program: info
                        .bsi
                        .dual_mono_second_program
                        .as_ref()
                        .and_then(|program| program.compression_gain_word),
                },
                transient_pre_noise: info.audio_frame.transient_pre_noise,
            },
        }
    }

    /// Вид на кадр AC-3 (всегда шесть блоков, TPNP нет).
    pub(crate) fn from_ac3(
        info: &SyncframeInfo,
        positions: &'a [ChannelPosition],
        blocks: &'a [DecodedAudioBlock],
    ) -> Self {
        let bsi = &info.bsi;
        Self {
            coding_mode: bsi.audio_coding_mode,
            positions,
            blocks: blocks.get(..AUDIO_BLOCKS_PER_SYNCFRAME).unwrap_or(blocks),
            compression: CompressionWords {
                first_program: bsi.program.compression_gain_word,
                second_program: bsi
                    .dual_mono_second_program
                    .as_ref()
                    .and_then(|program| program.compression_gain_word),
            },
            transient_pre_noise: [None; MAX_FULL_BANDWIDTH_CHANNELS],
        }
    }

    /// Число полнополосных каналов (позиции без LFE).
    pub(crate) fn full_bandwidth_channels(&self) -> usize {
        self.coding_mode.full_bandwidth_channels()
    }
}
