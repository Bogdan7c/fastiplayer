//! Сопоставление позиций `ac3-decoder` нейтральным позициям и перестановка lanes.

use ac3_decoder::ChannelPosition::{
    self, Center, CenterSurround, Left, LeftCenter, LeftRearSurround, LeftSurround,
    LeftSurroundDirect, LeftTopSurround, LeftWide, LowFrequencyEffects, LowFrequencyEffects2,
    Right, RightCenter, RightRearSurround, RightSurround, RightSurroundDirect, RightTopSurround,
    RightWide, TopSurround, VerticalHeightCenter, VerticalHeightLeft, VerticalHeightRight,
};
use audio_core::AudioChannelPosition as Neutral;

use super::super::canonical_layout::{LayoutConverter, NeutralLayout, UnconvertibleLayout};

/// Позиции canonical lanes layout-а.
fn lane_positions(layout: &NeutralLayout) -> Vec<Neutral> {
    let layout = layout.layout();
    (0..layout.channel_count() as usize)
        .map(|lane| layout.position_at(lane).unwrap())
        .collect()
}

/// Строит перестановку и возвращает (позиции lanes, канал декодера каждого lane).
fn mapping(decoder_channels: &[ChannelPosition]) -> (Vec<Neutral>, Vec<usize>) {
    let layout = NeutralLayout::for_decoder_channels(decoder_channels).unwrap();
    (
        lane_positions(&layout),
        layout.decoder_channel_for_lane().to_vec(),
    )
}

#[test]
fn ac3_5_1_decode_order_is_permuted_to_canonical_side_layout() {
    // Т. 5.8 acmod 7 + LFE последним: L, C, R, Ls, Rs, LFE.
    let (positions, sources) = mapping(&[
        Left,
        Center,
        Right,
        LeftSurround,
        RightSurround,
        LowFrequencyEffects,
    ]);

    assert_eq!(
        positions,
        [
            Neutral::FrontLeft,
            Neutral::FrontRight,
            Neutral::FrontCenter,
            Neutral::LowFrequencyEffects,
            Neutral::SideLeft,
            Neutral::SideRight,
        ]
    );
    assert_eq!(sources, [0, 2, 1, 5, 3, 4]);
}

#[test]
fn mono_surround_and_rear_pair_map_to_rear_positions() {
    let (positions, _) = mapping(&[Left, Right, CenterSurround]);
    assert_eq!(
        positions,
        [Neutral::FrontLeft, Neutral::FrontRight, Neutral::RearCenter]
    );

    // E-AC-3 7.1: ядро 3/2+LFE и зависимый substream с Lrs/Rrs.
    let (positions, sources) = mapping(&[
        Left,
        Center,
        Right,
        LeftSurround,
        RightSurround,
        LowFrequencyEffects,
        LeftRearSurround,
        RightRearSurround,
    ]);
    assert_eq!(
        positions,
        [
            Neutral::FrontLeft,
            Neutral::FrontRight,
            Neutral::FrontCenter,
            Neutral::LowFrequencyEffects,
            Neutral::RearLeft,
            Neutral::RearRight,
            Neutral::SideLeft,
            Neutral::SideRight,
        ]
    );
    assert_eq!(sources, [0, 2, 1, 5, 6, 7, 3, 4]);
}

#[test]
fn surround_direct_pair_takes_free_side_positions() {
    let (positions, sources) =
        mapping(&[Left, Center, Right, LeftSurroundDirect, RightSurroundDirect]);

    assert_eq!(
        positions,
        [
            Neutral::FrontLeft,
            Neutral::FrontRight,
            Neutral::FrontCenter,
            Neutral::SideLeft,
            Neutral::SideRight,
        ]
    );
    assert_eq!(sources, [0, 2, 1, 3, 4]);
}

#[test]
fn surround_direct_pair_falls_back_to_rear_when_side_is_taken_regardless_of_order() {
    // Lsd/Rsd стоят в кадре раньше Ls/Rs, но Ls/Rs всё равно получают side:
    // фиксированные позиции раскладываются первыми.
    let (positions, sources) = mapping(&[
        Left,
        Right,
        LeftSurroundDirect,
        RightSurroundDirect,
        LeftSurround,
        RightSurround,
    ]);

    assert_eq!(
        positions,
        [
            Neutral::FrontLeft,
            Neutral::FrontRight,
            Neutral::RearLeft,
            Neutral::RearRight,
            Neutral::SideLeft,
            Neutral::SideRight,
        ]
    );
    assert_eq!(sources, [0, 1, 2, 3, 4, 5]);
}

#[test]
fn surround_direct_without_free_side_or_rear_is_unconvertible() {
    let decoder_channels = [
        Left,
        Right,
        LeftSurround,
        RightSurround,
        LeftRearSurround,
        RightRearSurround,
        LeftSurroundDirect,
        RightSurroundDirect,
    ];

    assert_eq!(
        NeutralLayout::for_decoder_channels(&decoder_channels),
        Err(UnconvertibleLayout {
            decoder_channels: decoder_channels.to_vec(),
        })
    );
}

#[test]
fn duplicate_or_empty_decoder_channels_are_unconvertible() {
    assert!(NeutralLayout::for_decoder_channels(&[Left, Left]).is_err());
    assert!(NeutralLayout::for_decoder_channels(&[]).is_err());
}

#[test]
fn every_fixed_position_has_its_own_neutral_speaker() {
    // Все позиции Т. E2.5 кроме Lsd/Rsd разом: сопоставление инъективно, ни одна
    // позиция не теряется и не накладывается на другую.
    let all_fixed = [
        Left,
        Center,
        Right,
        LeftSurround,
        RightSurround,
        CenterSurround,
        LeftCenter,
        RightCenter,
        LeftRearSurround,
        RightRearSurround,
        TopSurround,
        LeftWide,
        RightWide,
        VerticalHeightLeft,
        VerticalHeightRight,
        VerticalHeightCenter,
        LeftTopSurround,
        RightTopSurround,
        LowFrequencyEffects2,
        LowFrequencyEffects,
    ];

    let (positions, sources) = mapping(&all_fixed);

    assert_eq!(positions.len(), all_fixed.len());
    let mut sorted_sources = sources.clone();
    sorted_sources.sort_unstable();
    assert_eq!(sorted_sources, (0..all_fixed.len()).collect::<Vec<_>>());
    for (decoder_position, neutral) in [
        (LeftWide, Neutral::FrontLeftWide),
        (VerticalHeightCenter, Neutral::TopFrontCenter),
        (LeftTopSurround, Neutral::TopSideLeft),
        (TopSurround, Neutral::TopCenter),
        (LowFrequencyEffects2, Neutral::LowFrequencyEffects2),
        (RightCenter, Neutral::FrontRightOfCenter),
    ] {
        let decoder_index = all_fixed
            .iter()
            .position(|position| *position == decoder_position)
            .unwrap();
        let lane = positions
            .iter()
            .position(|position| *position == neutral)
            .unwrap();
        assert_eq!(sources[lane], decoder_index, "{decoder_position:?}");
    }
}

#[test]
fn converter_recomputes_only_when_the_frame_layout_changes() {
    let mut converter = LayoutConverter::default();
    let stereo = converter.conversion_for(&[Left, Right]).clone().unwrap();
    let stereo_again = converter.conversion_for(&[Left, Right]).clone().unwrap();
    let five = converter
        .conversion_for(&[Left, Center, Right, LeftSurround, RightSurround])
        .clone()
        .unwrap();
    let unconvertible = converter.conversion_for(&[Left, Left]).clone().unwrap_err();

    assert_eq!(stereo, stereo_again);
    assert_eq!(stereo.layout().channel_count(), 2);
    assert_eq!(five.layout().channel_count(), 5);
    assert_eq!(unconvertible.decoder_channels, [Left, Left]);
}
