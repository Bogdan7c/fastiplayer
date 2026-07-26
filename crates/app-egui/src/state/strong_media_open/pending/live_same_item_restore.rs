//! S35S app-side intent/outcome routing без доступа к provider DVR state.

/// Timeline semantics, с которыми app ожидает authoritative position outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PositionRestoreTimeline {
    /// Обычный static restore может публиковать seekable/non-seekable checkpoint.
    Static,
    /// Live same-item restore никогда не создаёт persistent position checkpoint.
    LiveSameItem,
}

/// Correlated position restore facts остаются одним typed phase payload-ом.
pub(super) struct PendingPositionRestore {
    pub(super) requested_position: std::time::Duration,
    pub(super) timeline: PositionRestoreTimeline,
    pub(super) receipt: player_core::InstalledMediaStateRestoreReceipt,
}

/// App-owned route после authoritative player position outcome-а.
pub(super) enum PositionRestoreOutcomeRoute {
    /// Restore завершён и можно reassert playback intent с typed checkpoint semantics.
    Resume {
        checkpoint_position: crate::playlist_runtime::InstalledCheckpointPosition,
        warning: Option<crate::playlist_runtime::ResumePositionWarning>,
    },
    /// Outcome не принадлежит ожидаемому instance/timeline и остаётся fatal для transaction.
    Fail(player_core::InstalledMediaStateRestoreOutcome),
}

/// App выбирает только intent; fresh DVR membership остаётся player-owned.
pub(super) fn same_lineage_position_restore(
    timeline_mode: media_core::TimelineMode,
    _previous_absolute_position: std::time::Duration,
) -> (
    player_core::InstalledPositionRestore,
    PositionRestoreTimeline,
) {
    let timeline = match timeline_mode {
        media_core::TimelineMode::Live => PositionRestoreTimeline::LiveSameItem,
        media_core::TimelineMode::Static => PositionRestoreTimeline::Static,
    };
    (
        player_core::InstalledPositionRestore::AdoptPreparedSameLineagePosition,
        timeline,
    )
}

/// Проверяет exact instance/timeline и не превращает live outcome в persistent checkpoint.
pub(super) fn route_position_restore_outcome(
    outcome: player_core::InstalledMediaStateRestoreOutcome,
    expected_media_instance_id: player_core::MediaInstanceId,
    _requested_position: std::time::Duration,
    applied_position: std::time::Duration,
    timeline: PositionRestoreTimeline,
) -> PositionRestoreOutcomeRoute {
    match outcome {
        player_core::InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
            if media_instance_id == expected_media_instance_id =>
        {
            let checkpoint_position = match timeline {
                PositionRestoreTimeline::Static => {
                    crate::playlist_runtime::InstalledCheckpointPosition::Seekable(applied_position)
                }
                PositionRestoreTimeline::LiveSameItem => {
                    crate::playlist_runtime::InstalledCheckpointPosition::Live
                }
            };
            PositionRestoreOutcomeRoute::Resume {
                checkpoint_position,
                warning: None,
            }
        }
        player_core::InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
            media_instance_id,
            ..
        } if media_instance_id == expected_media_instance_id
            && timeline == PositionRestoreTimeline::LiveSameItem =>
        {
            PositionRestoreOutcomeRoute::Resume {
                checkpoint_position: crate::playlist_runtime::InstalledCheckpointPosition::Live,
                warning: None,
            }
        }
        player_core::InstalledMediaStateRestoreOutcome::PositionUnavailable {
            media_instance_id,
            requested_position,
            available_position,
            ..
        } if media_instance_id == expected_media_instance_id
            && timeline == PositionRestoreTimeline::Static =>
        {
            PositionRestoreOutcomeRoute::Resume {
                checkpoint_position:
                    crate::playlist_runtime::InstalledCheckpointPosition::NonSeekable,
                warning: Some(crate::playlist_runtime::ResumePositionWarning {
                    requested_position,
                    available_position,
                }),
            }
        }
        outcome => PositionRestoreOutcomeRoute::Fail(outcome),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_same_lineage_restore_delegates_range_decision_to_player() {
        let previous_absolute_position = std::time::Duration::from_secs(73);

        assert_eq!(
            same_lineage_position_restore(
                media_core::TimelineMode::Live,
                previous_absolute_position,
            ),
            (
                player_core::InstalledPositionRestore::AdoptPreparedSameLineagePosition,
                PositionRestoreTimeline::LiveSameItem,
            )
        );
        assert_eq!(
            same_lineage_position_restore(
                media_core::TimelineMode::Live,
                std::time::Duration::ZERO,
            ),
            (
                player_core::InstalledPositionRestore::AdoptPreparedSameLineagePosition,
                PositionRestoreTimeline::LiveSameItem,
            )
        );
    }

    #[test]
    fn live_restore_outcomes_keep_live_checkpoint_and_exact_instance_fence() {
        let media_instance_id = player_core::MediaInstanceId::from_non_zero(
            std::num::NonZeroU64::new(81).expect("test instance id"),
        );
        let stale_instance_id = player_core::MediaInstanceId::from_non_zero(
            std::num::NonZeroU64::new(82).expect("stale instance id"),
        );
        let requested_position = std::time::Duration::from_secs(73);

        for outcome in [
            player_core::InstalledMediaStateRestoreOutcome::Applied { media_instance_id },
            player_core::InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
                media_instance_id,
                requested_position,
                live_edge: std::time::Duration::from_secs(90),
                reason: player_core::InstalledLiveEdgeAdjustmentReason::DvrWindowUnavailable,
            },
        ] {
            assert!(matches!(
                route_position_restore_outcome(
                    outcome,
                    media_instance_id,
                    requested_position,
                    requested_position,
                    PositionRestoreTimeline::LiveSameItem,
                ),
                PositionRestoreOutcomeRoute::Resume {
                    checkpoint_position: crate::playlist_runtime::InstalledCheckpointPosition::Live,
                    warning: None,
                }
            ));
        }

        let stale_outcome = player_core::InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
            media_instance_id: stale_instance_id,
            requested_position,
            live_edge: std::time::Duration::from_secs(90),
            reason: player_core::InstalledLiveEdgeAdjustmentReason::DvrWindowUnavailable,
        };
        assert!(matches!(
            route_position_restore_outcome(
                stale_outcome,
                media_instance_id,
                requested_position,
                requested_position,
                PositionRestoreTimeline::LiveSameItem,
            ),
            PositionRestoreOutcomeRoute::Fail(
                player_core::InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
                    media_instance_id: failed_instance,
                    ..
                }
            ) if failed_instance == stale_instance_id
        ));
    }
}
