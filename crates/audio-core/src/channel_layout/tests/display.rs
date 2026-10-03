//! Текстовое представление раскладки каналов (логи, diagnostics, UI).
//!
//! Позиционная раскладка печатается в каноническом порядке lane-ов ровно по
//! одному разу на канал, дискретная — только числом каналов.

use super::*;

/// Все физические позиции в каноническом порядке lane-ов (порядок битов маски).
const ALL_POSITIONS_IN_LANE_ORDER: [AudioChannelPosition; 26] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontRight,
    AudioChannelPosition::FrontCenter,
    AudioChannelPosition::LowFrequencyEffects,
    AudioChannelPosition::RearLeft,
    AudioChannelPosition::RearRight,
    AudioChannelPosition::FrontLeftOfCenter,
    AudioChannelPosition::FrontRightOfCenter,
    AudioChannelPosition::RearCenter,
    AudioChannelPosition::SideLeft,
    AudioChannelPosition::SideRight,
    AudioChannelPosition::TopCenter,
    AudioChannelPosition::TopFrontLeft,
    AudioChannelPosition::TopFrontCenter,
    AudioChannelPosition::TopFrontRight,
    AudioChannelPosition::TopRearLeft,
    AudioChannelPosition::TopRearCenter,
    AudioChannelPosition::TopRearRight,
    AudioChannelPosition::LowFrequencyEffects2,
    AudioChannelPosition::TopSideLeft,
    AudioChannelPosition::TopSideRight,
    AudioChannelPosition::BottomFrontCenter,
    AudioChannelPosition::BottomFrontLeft,
    AudioChannelPosition::BottomFrontRight,
    AudioChannelPosition::FrontLeftWide,
    AudioChannelPosition::FrontRightWide,
];

#[test]
fn common_layouts_display_channels_in_lane_order() {
    assert_eq!(
        AudioChannelLayout::stereo().to_string(),
        "positioned[FrontLeft,FrontRight]"
    );
    assert_eq!(
        AudioChannelLayout::surround_5_1().to_string(),
        "positioned[FrontLeft,FrontRight,FrontCenter,LowFrequencyEffects,RearLeft,RearRight]"
    );
    assert_eq!(
        AudioChannelLayout::discrete(3)
            .expect("discrete layout")
            .to_string(),
        "discrete(3)"
    );
}

#[test]
fn full_positional_layout_maps_every_lane_to_its_position_once() {
    // Порядок входа намеренно обратный: раскладка обязана нормализовать его.
    let mut reversed = ALL_POSITIONS_IN_LANE_ORDER;
    reversed.reverse();
    let layout = AudioChannelLayout::positioned(&reversed).expect("all positions are distinct");

    assert_eq!(layout.channel_count(), 26);
    for (lane_index, expected) in ALL_POSITIONS_IN_LANE_ORDER.iter().enumerate() {
        assert_eq!(
            layout.position_at(lane_index),
            Some(*expected),
            "lane {lane_index}"
        );
        assert!(layout.contains(*expected));
    }
    assert_eq!(layout.position_at(ALL_POSITIONS_IN_LANE_ORDER.len()), None);

    let expected_display = format!(
        "positioned[{}]",
        ALL_POSITIONS_IN_LANE_ORDER
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_eq!(layout.to_string(), expected_display);
}
