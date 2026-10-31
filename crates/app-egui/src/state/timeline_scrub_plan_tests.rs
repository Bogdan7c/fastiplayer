//! Правила плана таймлайна: обычное окно — как всегда, браузерное — «кадры бегут
//! по полке».

use std::time::Duration;

use media_core::MediaTime;
use player_core::ScrubCommitPolicy;

use super::*;
use crate::downloaded_range::DownloadedRange;
use crate::downloaded_range::test_support::FakeDownloadedSpans;

/// Скачано 0–10 с и 20–30 с.
fn downloaded() -> DownloadedRange {
    FakeDownloadedSpans::range(vec![
        Duration::from_secs(0)..Duration::from_secs(10),
        Duration::from_secs(20)..Duration::from_secs(30),
    ])
    .1
}

const ON_SHELF: MediaTime = MediaTime::from_secs(5);
const OFF_SHELF: MediaTime = MediaTime::from_secs(15);

fn plan(
    action: TimelineAction,
    scope: LiveScrubScope,
    standing: PlayerScrubStanding,
    downloaded: &DownloadedRange,
) -> Vec<TimelineStep> {
    plan_timeline_action(
        action,
        &TimelinePlanContext {
            scope,
            standing,
            downloaded,
        },
    )
}

fn browser(action: TimelineAction, standing: PlayerScrubStanding) -> Vec<TimelineStep> {
    plan(
        action,
        LiveScrubScope::DownloadedRangeOnly,
        standing,
        &downloaded(),
    )
}

fn end_keep_latest() -> TimelineStep {
    TimelineStep::EndLiveScrub {
        policy: ScrubCommitPolicy::CommitLatestTarget,
        release: ScrubRelease::KeepLatestDispatched,
    }
}

fn viewer_seek(target: MediaTime) -> TimelineStep {
    TimelineStep::ExactSeek {
        target,
        route: TimelineCommandRoute::ReleaseOutsideDownloaded,
    }
}

#[test]
fn the_standard_window_maps_every_intent_to_the_player_as_before() {
    // Скачанное обычному окну не важно: даже «ничего не известно» не мешает скрабу.
    let unknown = DownloadedRange::Unknown;
    for standing in [
        PlayerScrubStanding::NotStarted,
        PlayerScrubStanding::Started,
    ] {
        let standard = |action| plan(action, LiveScrubScope::Anywhere, standing, &unknown);
        assert_eq!(
            standard(TimelineAction::BeginLiveScrub(OFF_SHELF)),
            [TimelineStep::BeginLiveScrub(OFF_SHELF)]
        );
        assert_eq!(
            standard(TimelineAction::PreviewLiveScrub(OFF_SHELF)),
            [TimelineStep::PreviewLiveScrub(OFF_SHELF)]
        );
        assert_eq!(
            standard(TimelineAction::EndLiveScrubAtVisiblePreview(OFF_SHELF)),
            [TimelineStep::EndLiveScrub {
                policy: ScrubCommitPolicy::CommitVisiblePreview,
                release: ScrubRelease::LandExactlyAt(OFF_SHELF),
            }]
        );
        assert_eq!(
            standard(TimelineAction::EndLiveScrubAtLatestTarget(OFF_SHELF)),
            [TimelineStep::EndLiveScrub {
                policy: ScrubCommitPolicy::CommitLatestTarget,
                release: ScrubRelease::LandExactlyAt(OFF_SHELF),
            }]
        );
        assert_eq!(
            standard(TimelineAction::CancelLiveScrub),
            [end_keep_latest()]
        );
    }
}

#[test]
fn begin_off_the_shelf_waits_and_the_first_on_shelf_target_begins_the_scrub() {
    assert!(
        browser(
            TimelineAction::BeginLiveScrub(OFF_SHELF),
            PlayerScrubStanding::NotStarted
        )
        .is_empty()
    );
    assert!(
        browser(
            TimelineAction::PreviewLiveScrub(OFF_SHELF),
            PlayerScrubStanding::NotStarted
        )
        .is_empty()
    );
    assert_eq!(
        browser(
            TimelineAction::PreviewLiveScrub(ON_SHELF),
            PlayerScrubStanding::NotStarted
        ),
        [TimelineStep::BeginLiveScrub(ON_SHELF)]
    );
    assert_eq!(
        browser(
            TimelineAction::BeginLiveScrub(ON_SHELF),
            PlayerScrubStanding::NotStarted
        ),
        [TimelineStep::BeginLiveScrub(ON_SHELF)]
    );
}

#[test]
fn during_the_scrub_only_on_shelf_targets_reach_the_player() {
    assert_eq!(
        browser(
            TimelineAction::PreviewLiveScrub(ON_SHELF),
            PlayerScrubStanding::Started
        ),
        [TimelineStep::PreviewLiveScrub(ON_SHELF)]
    );
    // Вне полки: кадр стоит, бегунок едет — плееру ничего (иначе — просьба странице).
    assert!(
        browser(
            TimelineAction::PreviewLiveScrub(OFF_SHELF),
            PlayerScrubStanding::Started
        )
        .is_empty()
    );
    // Второй кусок полки — снова кадры.
    let second_piece = MediaTime::from_secs(25);
    assert_eq!(
        browser(
            TimelineAction::PreviewLiveScrub(second_piece),
            PlayerScrubStanding::Started
        ),
        [TimelineStep::PreviewLiveScrub(second_piece)]
    );
}

#[test]
fn release_on_the_shelf_lands_locally_with_the_gesture_policy() {
    assert_eq!(
        browser(
            TimelineAction::EndLiveScrubAtVisiblePreview(ON_SHELF),
            PlayerScrubStanding::Started
        ),
        [TimelineStep::EndLiveScrub {
            policy: ScrubCommitPolicy::CommitVisiblePreview,
            release: ScrubRelease::LandExactlyAt(ON_SHELF),
        }]
    );
    // Весь жест был вне полки, отпустили на ней: начать и сразу сесть — без перемотки.
    assert_eq!(
        browser(
            TimelineAction::EndLiveScrubAtLatestTarget(ON_SHELF),
            PlayerScrubStanding::NotStarted
        ),
        [
            TimelineStep::BeginLiveScrub(ON_SHELF),
            TimelineStep::EndLiveScrub {
                policy: ScrubCommitPolicy::CommitLatestTarget,
                release: ScrubRelease::LandExactlyAt(ON_SHELF),
            },
        ]
    );
}

#[test]
fn release_off_the_shelf_ends_on_the_latest_shown_target_and_seeks_once() {
    for release in [
        TimelineAction::EndLiveScrubAtVisiblePreview(OFF_SHELF),
        TimelineAction::EndLiveScrubAtLatestTarget(OFF_SHELF),
    ] {
        assert_eq!(
            browser(release, PlayerScrubStanding::Started),
            [end_keep_latest(), viewer_seek(OFF_SHELF)],
            "{release:?}"
        );
        // Скраб у плеера не начинался: только одна перемотка.
        assert_eq!(
            browser(release, PlayerScrubStanding::NotStarted),
            [viewer_seek(OFF_SHELF)],
            "{release:?}"
        );
    }
}

#[test]
fn cancel_ends_only_a_started_scrub_and_never_seeks() {
    assert_eq!(
        browser(
            TimelineAction::CancelLiveScrub,
            PlayerScrubStanding::Started
        ),
        [end_keep_latest()]
    );
    assert!(
        browser(
            TimelineAction::CancelLiveScrub,
            PlayerScrubStanding::NotStarted
        )
        .is_empty()
    );
}

#[test]
fn without_a_known_downloaded_range_the_browser_window_only_seeks_on_release() {
    let unknown = DownloadedRange::Unknown;
    let unknown_plan = |action| {
        plan(
            action,
            LiveScrubScope::DownloadedRangeOnly,
            PlayerScrubStanding::NotStarted,
            &unknown,
        )
    };
    assert!(unknown_plan(TimelineAction::BeginLiveScrub(ON_SHELF)).is_empty());
    assert!(unknown_plan(TimelineAction::PreviewLiveScrub(ON_SHELF)).is_empty());
    assert_eq!(
        unknown_plan(TimelineAction::EndLiveScrubAtVisiblePreview(ON_SHELF)),
        [viewer_seek(ON_SHELF)]
    );
}

#[test]
fn clicks_and_plain_drags_stay_exact_seeks_in_both_windows() {
    for scope in [
        LiveScrubScope::Anywhere,
        LiveScrubScope::DownloadedRangeOnly,
    ] {
        let window = |action| {
            plan(
                action,
                scope,
                PlayerScrubStanding::NotStarted,
                &downloaded(),
            )
        };
        assert_eq!(
            window(TimelineAction::ClickSeek(OFF_SHELF)),
            [TimelineStep::ExactSeek {
                target: OFF_SHELF,
                route: TimelineCommandRoute::ClickSeek,
            }]
        );
        assert_eq!(
            window(TimelineAction::CommitDragSeek(ON_SHELF)),
            [TimelineStep::ExactSeek {
                target: ON_SHELF,
                route: TimelineCommandRoute::DragSeek,
            }]
        );
    }
}
