//! Подбор дорожки по подсказке: точное, примерное и отсутствие совпадения.

use super::super::*;

fn candidate(
    language: Option<&str>,
    title: Option<&str>,
    codec: &str,
    ordinal: u32,
) -> TrackHintCandidate {
    TrackHintCandidate {
        language: language.map(str::to_owned),
        title: title.map(str::to_owned),
        codec_id: codec.to_owned(),
        ordinal_among_kind: ordinal,
    }
}

fn remembered(
    language: Option<&str>,
    title: Option<&str>,
    codec: &str,
    ordinal: u32,
) -> RememberedTrackHint {
    RememberedTrackHint::new(
        language.map(str::to_owned),
        title.map(str::to_owned),
        codec.to_owned(),
        ordinal,
    )
    .expect("valid hint")
}

#[test]
fn exact_match_picks_the_candidate_with_every_field_equal() {
    let hint = remembered(Some("jpn"), Some("Dub"), "A_AC3", 1);
    let candidates = [
        candidate(Some("eng"), Some("Dub"), "A_AC3", 0),
        candidate(Some("jpn"), Some("Dub"), "A_AC3", 1),
    ];

    assert_eq!(
        find_matching_track(&hint, &candidates),
        TrackHintMatch::Exact(1)
    );
}

#[test]
fn language_comparison_ignores_ascii_case() {
    let hint = remembered(Some("JPN"), None, "A_AC3", 0);
    let candidates = [candidate(Some("jpn"), None, "A_AC3", 0)];

    assert_eq!(
        find_matching_track(&hint, &candidates),
        TrackHintMatch::Exact(0)
    );
}

#[test]
fn renumbered_track_matches_approximately_and_prefers_the_closest_ordinal() {
    let hint = remembered(Some("jpn"), Some("Dub"), "A_AC3", 1);
    let candidates = [
        candidate(Some("eng"), Some("Dub"), "A_AC3", 0),
        candidate(Some("jpn"), Some("Dub"), "A_AC3", 4),
        candidate(Some("jpn"), Some("Dub"), "A_AC3", 2),
    ];

    assert_eq!(
        find_matching_track(&hint, &candidates),
        TrackHintMatch::Approximate(2)
    );
}

#[test]
fn renamed_track_still_matches_by_language_and_codec() {
    let hint = remembered(Some("jpn"), Some("Old title"), "A_AC3", 1);
    let candidates = [
        candidate(Some("eng"), Some("Old title"), "A_AC3", 1),
        candidate(Some("jpn"), Some("New title"), "A_AC3", 3),
    ];

    assert_eq!(
        find_matching_track(&hint, &candidates),
        TrackHintMatch::Approximate(1)
    );
}

#[test]
fn language_title_codec_beats_language_codec() {
    let hint = remembered(Some("jpn"), Some("Dub"), "A_AC3", 1);
    let candidates = [
        candidate(Some("jpn"), Some("Other"), "A_AC3", 1),
        candidate(Some("jpn"), Some("Dub"), "A_AC3", 5),
    ];

    assert_eq!(
        find_matching_track(&hint, &candidates),
        TrackHintMatch::Approximate(1)
    );
}

#[test]
fn different_language_is_never_picked_even_with_same_codec_and_ordinal() {
    let hint = remembered(Some("jpn"), Some("Dub"), "A_AC3", 0);
    let candidates = [
        candidate(Some("eng"), Some("Dub"), "A_AC3", 0),
        candidate(None, Some("Dub"), "A_AC3", 0),
    ];

    assert_eq!(
        find_matching_track(&hint, &candidates),
        TrackHintMatch::NotFound
    );
}

#[test]
fn different_codec_is_not_found() {
    let hint = remembered(Some("jpn"), None, "A_AC3", 0);
    let candidates = [candidate(Some("jpn"), None, "A_DTS", 0)];

    assert_eq!(
        find_matching_track(&hint, &candidates),
        TrackHintMatch::NotFound
    );
}

#[test]
fn untagged_hint_only_matches_exactly() {
    let hint = remembered(None, None, "A_AAC", 2);
    let moved = [candidate(None, None, "A_AAC", 0)];
    let same = [
        candidate(None, None, "A_AAC", 0),
        candidate(None, None, "A_AAC", 2),
    ];

    assert_eq!(find_matching_track(&hint, &moved), TrackHintMatch::NotFound);
    assert_eq!(find_matching_track(&hint, &same), TrackHintMatch::Exact(1));
}

#[test]
fn identical_candidates_resolve_to_the_first_deterministically() {
    let hint = remembered(Some("eng"), Some("Main"), "S_TEXT/UTF8", 3);
    let twin = candidate(Some("eng"), Some("Main"), "S_TEXT/UTF8", 3);
    let candidates = [twin.clone(), twin];

    assert_eq!(
        find_matching_track(&hint, &candidates),
        TrackHintMatch::Exact(0)
    );
    assert_eq!(find_matching_track(&hint, &[]), TrackHintMatch::NotFound);
}
