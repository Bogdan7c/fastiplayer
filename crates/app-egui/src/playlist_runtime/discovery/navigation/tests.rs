use playlist_core::ManualNavigationDirection;
use playlist_discovery::AdmissionDirection;

use super::{accept_monotonic_revision, admission_direction_matches};

#[test]
fn latest_frontier_revision_rejects_duplicate_and_out_of_order_completion() {
    let mut revision = 0;
    assert!(accept_monotonic_revision(&mut revision, 3));
    assert!(!accept_monotonic_revision(&mut revision, 2));
    assert!(!accept_monotonic_revision(&mut revision, 3));
    assert!(accept_monotonic_revision(&mut revision, 5));
    assert_eq!(revision, 5);
}

#[test]
fn non_shuffle_direction_never_accepts_opposite_or_nondirectional_frontier() {
    assert!(admission_direction_matches(
        ManualNavigationDirection::Next,
        AdmissionDirection::After,
    ));
    assert!(admission_direction_matches(
        ManualNavigationDirection::Previous,
        AdmissionDirection::Before,
    ));
    assert!(!admission_direction_matches(
        ManualNavigationDirection::Next,
        AdmissionDirection::Before,
    ));
    assert!(!admission_direction_matches(
        ManualNavigationDirection::Previous,
        AdmissionDirection::NonDirectional,
    ));
}
