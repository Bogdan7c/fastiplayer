//! Публичный разбор access unit-а «без PCM»: состав substream-ов программы 0,
//! позиции кодированных каналов и использованные инструменты Annex E.
//!
//! Нужен диагностике и адаптеру до/без синтеза PCM: тот же разбор, что
//! делает декодер (CRC по политике, синтаксис, учёт бит), но наружу отдаются
//! только заголовки, раскладки каналов и счётчики инструментов.

use super::access_unit::{EnhancedAccessUnitParser, ParsedSubstream, SubstreamHeader};
use super::tool_usage::EnhancedToolUsage;
use crate::config::DecoderConfig;
use crate::error::Ac3DecodeError;
use crate::output::ChannelPosition;
use crate::spectral::dither::DitherGenerator;
use crate::syntax::syncframe::AUDIO_BLOCKS_PER_SYNCFRAME;

/// Один substream программы 0.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SubstreamInspection {
    /// Заголовок кадра (AC-3 или E-AC-3).
    pub header: SubstreamHeader,
    /// Позиции кодированных каналов в порядке потока (fbw, затем LFE).
    pub channels: Vec<ChannelPosition>,
    /// Наибольший кодированный бин (+1) каждого канала за кадр, в порядке
    /// `channels`: полоса канала с учётом coupling и SPX (бин `k` ≈ `(k + ½)·fs/512`).
    pub bandwidth_bins: Vec<usize>,
    /// Инструменты, которые кадр реально использовал.
    pub tools: EnhancedToolUsage,
}

/// Разобранный access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AccessUnitInspection {
    /// Независимый substream 0 (или ядро AC-3), затем его зависимые.
    pub substreams: Vec<SubstreamInspection>,
    /// Пропущенные syncframe-ы других программ (substreamid 1–7).
    pub skipped_substreams: usize,
}

/// Полностью разбирает access unit (до частотных коэффициентов) и описывает его.
pub(crate) fn inspect_access_unit(
    access_unit: &[u8],
    config: DecoderConfig,
) -> Result<AccessUnitInspection, Ac3DecodeError> {
    let mut parser = EnhancedAccessUnitParser::new(config.crc);
    let mut dither = DitherGenerator::new(config.dither);
    let parsed = parser.parse(access_unit, &mut dither)?;
    Ok(AccessUnitInspection {
        substreams: parsed
            .substreams()
            .map(|substream| SubstreamInspection {
                header: substream.header.clone(),
                channels: substream.positions.to_vec(),
                bandwidth_bins: bandwidth_bins(&substream),
                tools: substream.tools,
            })
            .collect(),
        skipped_substreams: parsed.skipped_substreams,
    })
}

/// Полоса каждого кодированного канала: максимум `end_bin` по блокам кадра
/// (fbw по порядку, затем LFE — как `positions`).
fn bandwidth_bins(substream: &ParsedSubstream<'_>) -> Vec<usize> {
    let (blocks, full_bandwidth, lfe_on) = match substream.header {
        SubstreamHeader::Ac3(info) => (
            AUDIO_BLOCKS_PER_SYNCFRAME,
            info.bsi.full_bandwidth_channels(),
            info.bsi.lfe_on,
        ),
        SubstreamHeader::Enhanced(info) => (
            info.audio_blocks,
            info.bsi.full_bandwidth_channels(),
            info.bsi.lfe_on,
        ),
    };
    let frame_blocks = || substream.blocks.iter().take(blocks);
    let mut bandwidths: Vec<usize> = (0..full_bandwidth)
        .map(|channel| {
            frame_blocks()
                .map(|block| {
                    block
                        .full_bandwidth
                        .get(channel)
                        .map_or(0, |spectrum| spectrum.end_bin)
                })
                .max()
                .unwrap_or(0)
        })
        .collect();
    if lfe_on {
        bandwidths.push(
            frame_blocks()
                .map(|block| block.lfe.end_bin)
                .max()
                .unwrap_or(0),
        );
    }
    bandwidths
}
