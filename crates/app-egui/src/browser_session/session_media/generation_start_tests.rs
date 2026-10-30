//! Модульные тесты правила посадки стартовой позиции (03e).

use std::time::Duration;

use media_core::{DemuxSeekResult, MediaTime};
use player_core::{PreparedDemuxSeekLandingPolicy, PreparedInitialPosition};

use super::{AfterLoad, GenerationStart, positioned_at};

fn time(millis: u64) -> MediaTime {
    MediaTime::from_duration(Duration::from_millis(millis))
}

/// Итог перемотки источника к `target` с посадкой на `landing`.
fn seek_result(target: MediaTime, landing: MediaTime) -> DemuxSeekResult {
    DemuxSeekResult {
        requested_position: target,
        actual_position: landing,
        actual_track_timestamp: None,
    }
}

fn landing_policy(position: PreparedInitialPosition) -> Option<PreparedDemuxSeekLandingPolicy> {
    match position {
        PreparedInitialPosition::Beginning => None,
        PreparedInitialPosition::PositionedAt { landing_policy, .. } => Some(landing_policy),
    }
}

#[test]
fn a_landing_before_the_target_decodes_forward_to_the_target() {
    let position = positioned_at(time(270), seek_result(time(270), time(244)));
    assert_eq!(
        landing_policy(position),
        Some(PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget)
    );
    assert!(position.validate().is_ok(), "{position:?}");
}

#[test]
fn a_landing_exactly_at_the_target_decodes_forward_to_the_target() {
    let position = positioned_at(time(270), seek_result(time(270), time(270)));
    assert_eq!(
        landing_policy(position),
        Some(PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget)
    );
    assert!(position.validate().is_ok(), "{position:?}");
}

/// Полка допускает коробку, начатую чуть позже цели: такая посадка — допустимая для
/// ядра позиция «с посадки», а не противоречие договора.
#[test]
fn a_landing_after_the_target_starts_from_the_landing() {
    let position = positioned_at(time(150), seek_result(time(150), time(200)));
    assert_eq!(
        landing_policy(position),
        Some(PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget)
    );
    assert!(position.validate().is_ok(), "{position:?}");
}

#[test]
fn a_file_is_seeked_after_its_load_only_when_the_page_named_a_position() {
    assert_eq!(
        GenerationStart::FirstCommonData.after_file_load(),
        AfterLoad::Nothing
    );
    let target = Duration::from_secs(30);
    assert_eq!(
        GenerationStart::PageSeekedTo(target).after_file_load(),
        AfterLoad::SeekTo(target)
    );
}
