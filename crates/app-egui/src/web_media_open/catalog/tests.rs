use super::complete_parent_choices;
use crate::web_media_catalog::{WebMediaCatalogChoice, WebMediaMode, WebMediaSelectionTarget};

fn fixture_choice(target: u64, rank: usize, mode: WebMediaMode) -> WebMediaCatalogChoice {
    WebMediaCatalogChoice {
        mode,
        video: None,
        rank: web_media_playback_plan::OpaqueAlternativeRank::parent(rank),
        target: WebMediaSelectionTarget::Fixture(target),
    }
}

#[test]
fn declared_catalog_keeps_every_planner_ranked_choice_without_probe_budget() {
    let active = WebMediaSelectionTarget::Fixture(12);
    let choices = (1..=12)
        .rev()
        .map(|target| fixture_choice(target, target as usize, WebMediaMode::VideoAndAudio))
        .collect();

    let complete = complete_parent_choices(choices, &active).unwrap();

    assert_eq!(complete.len(), 12);
    assert_eq!(
        complete.first().unwrap().target,
        WebMediaSelectionTarget::Fixture(1)
    );
    assert_eq!(complete.last().unwrap().target, active);
}

#[test]
fn declared_catalog_order_is_source_order_independent() {
    let active = WebMediaSelectionTarget::Fixture(4);
    let forward = vec![
        fixture_choice(1, 1, WebMediaMode::VideoAndAudio),
        fixture_choice(2, 2, WebMediaMode::VideoOnly),
        fixture_choice(3, 3, WebMediaMode::AudioOnly),
        fixture_choice(4, 4, WebMediaMode::VideoAndAudio),
    ];
    let mut reversed = forward.clone();
    reversed.reverse();

    assert_eq!(
        complete_parent_choices(forward, &active).unwrap(),
        complete_parent_choices(reversed, &active).unwrap()
    );
}

#[test]
fn declared_catalog_rejects_missing_active_choice() {
    let error = complete_parent_choices(
        vec![fixture_choice(1, 1, WebMediaMode::VideoAndAudio)],
        &WebMediaSelectionTarget::Fixture(2),
    )
    .expect_err("missing active choice должен fail closed");

    assert!(error.to_string().contains("active Installed choice"));
}

#[test]
fn declared_catalog_projection_has_no_candidate_or_provider_io() {
    let source = include_str!("../catalog.rs");

    for forbidden in [
        "open_candidate(",
        "discover_hls_candidate_catalog",
        "discover_dash_candidate_catalog",
        "discover_smooth_candidate_catalog",
        "discover_hds_candidate_catalog",
        "MAX_PARENT_CATALOG_CHOICES",
    ] {
        assert!(
            !source.contains(forbidden),
            "declared catalog projection не должна содержать `{forbidden}`"
        );
    }
}
