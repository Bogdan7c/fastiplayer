//! Сведение substream-ов программы в один набор выходных каналов
//! (A/52:2012 Annex E §3.8.2; ETSI TS 102 366 v1.4.1 §E.2.8.2).
//!
//! Правило спецификации: каналы зависимого substream-а, позиции которых уже
//! есть в программе, ЗАМЕНЯЮТ их (данные независимого substream-а 0 или
//! более раннего зависимого не используются), остальные — ДОБАВЛЯЮТСЯ
//! (раскладки больше 5.1). Без `chanmap` позиции зависимого задают его
//! `acmod`/`lfeon` — это тоже замена (пример §3.8.2: 1/0 + LFE заменяет C и
//! LFE). Позиции кодированных каналов каждого substream-а уже посчитаны
//! синтаксисом (`channel_map::substream_positions`).
//!
//! Порядок выхода — порядок позиций Таблицы E2.5 (L, C, R, Ls, Rs, Lc, Rc,
//! Lrs, Rrs, Cs, Ts, Lsd, Rsd, Lw, Rw, Vhl, Vhr, Vhc, Lts, Rts, LFE2), LFE
//! последним. Для одиночного AC-3/E-AC-3 это ровно порядок Таблицы 5.8 +
//! LFE (моно-surround S режимов 2/1 и 3/1 — `CenterSurround`), то есть
//! раскладка пакета D2 не меняется, а итог не зависит от того, как кодер
//! разложил каналы по substream-ам.

use super::substream::SubstreamFrame;
use crate::error::{Ac3DecodeError, AccessUnitViolation};
use crate::output::ChannelPosition;

/// Наибольшее число каналов одной программы (§3.8.2: «maximum number of
/// channels rendered for a single program is 16»).
pub(crate) const MAX_PROGRAM_CHANNELS: usize = 16;

/// Какой кодированный канал substream-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodedChannel {
    /// Полнополосный канал с этим номером (порядок кодирования).
    FullBandwidth(usize),
    /// Канал LFE (`lfeon`).
    LowFrequencyEffects,
}

/// Откуда берётся выходной канал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelSource {
    /// Номер substream-а в access unit-е (0 — независимый или ядро AC-3).
    pub(crate) substream: usize,
    pub(crate) coded: CodedChannel,
}

/// Выходной канал программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputChannel {
    pub(crate) position: ChannelPosition,
    pub(crate) source: ChannelSource,
}

/// Раскладка программы: выходные каналы в порядке Таблицы E2.5, LFE последним.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProgramLayout {
    channels: Vec<OutputChannel>,
}

impl ProgramLayout {
    /// Сводит substream-ы (первый — независимый) по §3.8.2.
    ///
    /// Ошибка — больше [`MAX_PROGRAM_CHANNELS`] разных позиций.
    pub(crate) fn combine(substreams: &[SubstreamFrame<'_>]) -> Result<Self, Ac3DecodeError> {
        let mut channels: Vec<OutputChannel> = Vec::with_capacity(MAX_PROGRAM_CHANNELS);
        for (substream_index, substream) in substreams.iter().enumerate() {
            let full_bandwidth = substream.full_bandwidth_channels();
            for (coded_index, &position) in substream.positions.iter().enumerate() {
                let coded = if coded_index < full_bandwidth {
                    CodedChannel::FullBandwidth(coded_index)
                } else {
                    CodedChannel::LowFrequencyEffects
                };
                let source = ChannelSource {
                    substream: substream_index,
                    coded,
                };
                match channels
                    .iter_mut()
                    .find(|channel| channel.position == position)
                {
                    // Позиция уже есть — данные более позднего substream-а заменяют её.
                    Some(existing) => existing.source = source,
                    None => channels.push(OutputChannel { position, source }),
                }
            }
        }
        if channels.len() > MAX_PROGRAM_CHANNELS {
            return Err(Ac3DecodeError::InvalidAccessUnit(
                AccessUnitViolation::TooManyProgramChannels,
            ));
        }
        // Устойчивая сортировка: позиции уникальны, порядок полностью задан рангом.
        channels.sort_by_key(|channel| program_order(channel.position));
        Ok(Self { channels })
    }

    /// Выходные каналы по порядку.
    pub(crate) fn channels(&self) -> &[OutputChannel] {
        &self.channels
    }

    /// Позиции выходных каналов по порядку.
    pub(crate) fn positions(&self) -> Vec<ChannelPosition> {
        self.channels
            .iter()
            .map(|channel| channel.position)
            .collect()
    }
}

/// Ранг позиции в выходе: номер бита Таблицы E2.5 (пары — первый и второй
/// канал пары), LFE (бит 15) — последним.
const fn program_order(position: ChannelPosition) -> u8 {
    use ChannelPosition::{
        Center, CenterSurround, Left, LeftCenter, LeftRearSurround, LeftSurround,
        LeftSurroundDirect, LeftTopSurround, LeftWide, LowFrequencyEffects, LowFrequencyEffects2,
        Right, RightCenter, RightRearSurround, RightSurround, RightSurroundDirect,
        RightTopSurround, RightWide, TopSurround, VerticalHeightCenter, VerticalHeightLeft,
        VerticalHeightRight,
    };
    match position {
        Left => 0,
        Center => 1,
        Right => 2,
        LeftSurround => 3,
        RightSurround => 4,
        LeftCenter => 5,
        RightCenter => 6,
        LeftRearSurround => 7,
        RightRearSurround => 8,
        CenterSurround => 9,
        TopSurround => 10,
        LeftSurroundDirect => 11,
        RightSurroundDirect => 12,
        LeftWide => 13,
        RightWide => 14,
        VerticalHeightLeft => 15,
        VerticalHeightRight => 16,
        VerticalHeightCenter => 17,
        LeftTopSurround => 18,
        RightTopSurround => 19,
        LowFrequencyEffects2 => 20,
        LowFrequencyEffects => 21,
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelSource, CodedChannel, MAX_PROGRAM_CHANNELS, ProgramLayout};
    use crate::error::{Ac3DecodeError, AccessUnitViolation};
    use crate::output::ChannelPosition::{
        self, Center, CenterSurround, Left, LeftRearSurround, LeftSurround, LeftWide,
        LowFrequencyEffects, Right, RightRearSurround, RightSurround, RightWide,
        VerticalHeightCenter,
    };
    use crate::output::ac3_channel_positions;
    use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
    use crate::syntax::bsi_values::AudioCodingMode;
    use crate::synthesis::program::substream::{CompressionWords, SubstreamFrame};

    fn frame(mode: AudioCodingMode, positions: &[ChannelPosition]) -> SubstreamFrame<'_> {
        SubstreamFrame {
            coding_mode: mode,
            positions,
            blocks: &[],
            compression: CompressionWords::default(),
            transient_pre_noise: [None; MAX_FULL_BANDWIDTH_CHANNELS],
        }
    }

    fn source(substream: usize, coded: CodedChannel) -> ChannelSource {
        ChannelSource { substream, coded }
    }

    #[test]
    fn single_substream_keeps_table_5_8_order_with_lfe_last() {
        for acmod in 0..8 {
            let mode = AudioCodingMode::from_code(acmod);
            let positions = ac3_channel_positions(mode, true);
            let layout = ProgramLayout::combine(&[frame(mode, &positions)]).unwrap();
            assert_eq!(layout.positions(), positions, "acmod {acmod}");
            // Каждый выходной канал — свой кодированный канал по порядку.
            for (index, channel) in layout.channels().iter().enumerate() {
                let expected = if index + 1 == positions.len() {
                    CodedChannel::LowFrequencyEffects
                } else {
                    CodedChannel::FullBandwidth(index)
                };
                assert_eq!(channel.source, source(0, expected));
            }
        }
    }

    /// Раскладка реальных 7.1-потоков Dolby: 3/2+LFE и зависимый с chanmap
    /// Ls, Rs, Lrs, Rrs — Ls/Rs заменяются, Lrs/Rrs добавляются.
    #[test]
    fn dependent_replaces_matching_positions_and_adds_new_ones() {
        let independent = ac3_channel_positions(AudioCodingMode::ThreeFrontStereoSurround, true);
        let dependent = [
            LeftSurround,
            RightSurround,
            LeftRearSurround,
            RightRearSurround,
        ];
        let layout = ProgramLayout::combine(&[
            frame(AudioCodingMode::ThreeFrontStereoSurround, &independent),
            frame(AudioCodingMode::StereoStereoSurround, &dependent),
        ])
        .unwrap();
        assert_eq!(
            layout.positions(),
            [
                Left,
                Center,
                Right,
                LeftSurround,
                RightSurround,
                LeftRearSurround,
                RightRearSurround,
                LowFrequencyEffects
            ]
        );
        let sources: Vec<_> = layout
            .channels()
            .iter()
            .map(|channel| channel.source)
            .collect();
        use CodedChannel::{FullBandwidth, LowFrequencyEffects as Lfe};
        assert_eq!(
            sources,
            [
                source(0, FullBandwidth(0)),
                source(0, FullBandwidth(1)),
                source(0, FullBandwidth(2)),
                source(1, FullBandwidth(0)),
                source(1, FullBandwidth(1)),
                source(1, FullBandwidth(2)),
                source(1, FullBandwidth(3)),
                source(0, Lfe),
            ]
        );
    }

    /// Пример §3.8.2: зависимый 1/0 + LFE без chanmap заменяет C и LFE;
    /// более поздний зависимый заменяет и данные более раннего.
    #[test]
    fn later_substreams_win_and_acmod_dependents_replace() {
        let independent = ac3_channel_positions(AudioCodingMode::ThreeFrontStereoSurround, true);
        let center_lfe = ac3_channel_positions(AudioCodingMode::Mono, true);
        let center_only = [Center];
        let layout = ProgramLayout::combine(&[
            frame(AudioCodingMode::ThreeFrontStereoSurround, &independent),
            frame(AudioCodingMode::Mono, &center_lfe),
            frame(AudioCodingMode::Mono, &center_only),
        ])
        .unwrap();
        assert_eq!(layout.positions(), independent);
        let find = |position| {
            layout
                .channels()
                .iter()
                .find(|channel| channel.position == position)
                .unwrap()
                .source
        };
        assert_eq!(find(Center), source(2, CodedChannel::FullBandwidth(0)));
        assert_eq!(
            find(LowFrequencyEffects),
            source(1, CodedChannel::LowFrequencyEffects)
        );
        assert_eq!(find(Left), source(0, CodedChannel::FullBandwidth(0)));
    }

    #[test]
    fn more_than_sixteen_program_channels_is_rejected() {
        let base = ac3_channel_positions(AudioCodingMode::ThreeFrontStereoSurround, true);
        let wide = [LeftWide, RightWide, VerticalHeightCenter, CenterSurround];
        let rear = [LeftRearSurround, RightRearSurround];
        let mut substreams = vec![frame(AudioCodingMode::ThreeFrontStereoSurround, &base)];
        substreams.push(frame(AudioCodingMode::StereoStereoSurround, &wide));
        substreams.push(frame(AudioCodingMode::Stereo, &rear));
        // 6 + 4 + 2 = 12 позиций — допустимо.
        assert_eq!(
            ProgramLayout::combine(&substreams)
                .unwrap()
                .channels()
                .len(),
            12
        );
        let extra = [
            ChannelPosition::LeftCenter,
            ChannelPosition::RightCenter,
            ChannelPosition::TopSurround,
            ChannelPosition::LeftSurroundDirect,
            ChannelPosition::RightSurroundDirect,
        ];
        substreams.push(frame(AudioCodingMode::ThreeFrontStereoSurround, &extra));
        assert!(12 + extra.len() > MAX_PROGRAM_CHANNELS);
        assert_eq!(
            ProgramLayout::combine(&substreams),
            Err(Ac3DecodeError::InvalidAccessUnit(
                AccessUnitViolation::TooManyProgramChannels
            ))
        );
    }
}
