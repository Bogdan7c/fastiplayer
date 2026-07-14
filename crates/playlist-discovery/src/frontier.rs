//! D43/D74 directional frontier: contiguous proof, quotas и exact-once release.

use crate::stream::{
    ADMITTED_BATCH_RECORD_LIMIT, AdmissionDirection, AdmissionSideAccounting, DiscoveryRecord,
};

/// Hard automatic sibling count без explicit target.
pub const AUTOMATIC_SIBLING_RECORD_LIMIT: usize = 49_999;

/// Базовая квота natural-before стороны.
pub const AUTOMATIC_SIBLING_BEFORE_QUOTA: usize = 24_999;

/// Базовая квота natural-after стороны; extra odd slot принадлежит Next стороне.
pub const AUTOMATIC_SIBLING_AFTER_QUOTA: usize = 25_000;

/// Максимум offsets одной стороны от текущего contiguous D74 cursor-а.
pub const DIRECTIONAL_LOOKAHEAD_LIMIT: usize = 256;

#[derive(Debug)]
pub(crate) enum TerminalCandidate {
    Eligible(Box<DiscoveryRecord>),
    Ineligible,
}

#[derive(Debug)]
struct DirectionalFrontier {
    outcomes: Vec<Option<TerminalCandidate>>,
    cursor: usize,
    admitted: usize,
    exhausted: bool,
    revision: u64,
}

impl DirectionalFrontier {
    fn new(candidate_count: usize) -> Self {
        Self {
            outcomes: std::iter::repeat_with(|| None)
                .take(candidate_count)
                .collect(),
            cursor: 0,
            admitted: 0,
            exhausted: candidate_count == 0,
            revision: 0,
        }
    }

    fn record_terminal(&mut self, offset: usize, outcome: TerminalCandidate) -> bool {
        let Some(slot) = self.outcomes.get_mut(offset) else {
            return false;
        };
        if slot.is_some() {
            return false;
        }
        *slot = Some(outcome);
        true
    }

    fn can_advance(&self) -> bool {
        self.outcomes.get(self.cursor).is_some_and(Option::is_some)
    }

    fn take_next(&mut self) -> Option<TerminalCandidate> {
        let outcome = self.outcomes.get_mut(self.cursor)?.take()?;
        self.cursor += 1;
        self.exhausted = self.cursor == self.outcomes.len();
        self.revision = self.revision.saturating_add(1);
        Some(outcome)
    }

    fn unused_base_quota(&self, base_quota: usize) -> usize {
        if self.exhausted {
            base_quota.saturating_sub(self.admitted)
        } else {
            0
        }
    }
}

/// Один contiguous release, который job оборачивает в `AdmittedBatch`.
#[derive(Debug)]
pub(crate) struct FrontierRelease {
    pub direction: AdmissionDirection,
    pub records: Vec<DiscoveryRecord>,
    pub revision: u64,
    pub exhausted: bool,
    pub side_accounting: Option<AdmissionSideAccounting>,
}

/// Job-owned D43/D74 state; app/domain ownership здесь отсутствует.
#[derive(Debug)]
pub(crate) struct SiblingAdmissionFrontier {
    before: DirectionalFrontier,
    after: DirectionalFrontier,
    total_admitted: usize,
    limit_reached: bool,
    limits: AdmissionLimits,
}

#[derive(Clone, Copy, Debug)]
struct AdmissionLimits {
    before: usize,
    after: usize,
    total: usize,
}

impl SiblingAdmissionFrontier {
    pub(crate) fn new(before_count: usize, after_count: usize) -> Self {
        Self::new_with_limits(
            before_count,
            after_count,
            AdmissionLimits {
                before: AUTOMATIC_SIBLING_BEFORE_QUOTA,
                after: AUTOMATIC_SIBLING_AFTER_QUOTA,
                total: AUTOMATIC_SIBLING_RECORD_LIMIT,
            },
        )
    }

    fn new_with_limits(before_count: usize, after_count: usize, limits: AdmissionLimits) -> Self {
        Self {
            before: DirectionalFrontier::new(before_count),
            after: DirectionalFrontier::new(after_count),
            total_admitted: 0,
            limit_reached: false,
            limits,
        }
    }

    pub(crate) fn record_terminal(
        &mut self,
        direction: AdmissionDirection,
        offset: usize,
        outcome: TerminalCandidate,
    ) -> bool {
        match direction {
            AdmissionDirection::Before => self.before.record_terminal(offset, outcome),
            AdmissionDirection::After => self.after.record_terminal(offset, outcome),
            AdmissionDirection::NonDirectional => false,
        }
    }

    /// Разрешает scheduling только внутри bounded окна конкретной стороны.
    pub(crate) fn can_schedule(&self, direction: AdmissionDirection, offset: usize) -> bool {
        let cursor = match direction {
            AdmissionDirection::Before => self.before.cursor,
            AdmissionDirection::After => self.after.cursor,
            AdmissionDirection::NonDirectional => return true,
        };
        offset >= cursor && offset < cursor.saturating_add(DIRECTIONAL_LOOKAHEAD_LIMIT)
    }

    pub(crate) fn release_contiguous(
        &mut self,
        mut available_record_slots: usize,
    ) -> Vec<FrontierRelease> {
        let mut releases = Vec::with_capacity(2);
        self.release_direction(
            AdmissionDirection::Before,
            &mut available_record_slots,
            &mut releases,
        );
        self.release_direction(
            AdmissionDirection::After,
            &mut available_record_slots,
            &mut releases,
        );

        // Exhaustion одной стороны может расширить уже остановившуюся другую.
        self.release_direction(
            AdmissionDirection::Before,
            &mut available_record_slots,
            &mut releases,
        );
        self.release_direction(
            AdmissionDirection::After,
            &mut available_record_slots,
            &mut releases,
        );
        self.limit_reached = self.total_admitted == self.limits.total;
        releases
    }

    fn release_direction(
        &mut self,
        direction: AdmissionDirection,
        available_record_slots: &mut usize,
        releases: &mut Vec<FrontierRelease>,
    ) {
        let effective_quota = self.effective_quota(direction);
        let frontier = match direction {
            AdmissionDirection::Before => &mut self.before,
            AdmissionDirection::After => &mut self.after,
            AdmissionDirection::NonDirectional => return,
        };
        let old_revision = frontier.revision;
        let mut released_records = Vec::new();

        while frontier.can_advance() {
            let next_is_eligible = matches!(
                frontier.outcomes[frontier.cursor],
                Some(TerminalCandidate::Eligible(_))
            );
            if next_is_eligible
                && (frontier.admitted == effective_quota
                    || self.total_admitted == self.limits.total
                    || released_records.len() == ADMITTED_BATCH_RECORD_LIMIT
                    || *available_record_slots == 0)
            {
                break;
            }
            let Some(outcome) = frontier.take_next() else {
                break;
            };
            if let TerminalCandidate::Eligible(record) = outcome {
                frontier.admitted += 1;
                self.total_admitted += 1;
                *available_record_slots -= 1;
                released_records.push(*record);
            }
        }

        if frontier.revision != old_revision {
            releases.push(FrontierRelease {
                direction,
                records: released_records,
                revision: frontier.revision,
                exhausted: frontier.exhausted,
                side_accounting: Some(AdmissionSideAccounting {
                    admitted_on_side: frontier.admitted,
                    effective_side_quota: effective_quota,
                    total_admitted: self.total_admitted,
                }),
            });
        }
    }

    fn effective_quota(&self, direction: AdmissionDirection) -> usize {
        match direction {
            AdmissionDirection::Before => {
                self.limits.before + self.after.unused_base_quota(self.limits.after)
            }
            AdmissionDirection::After => {
                self.limits.after + self.before.unused_base_quota(self.limits.before)
            }
            AdmissionDirection::NonDirectional => 0,
        }
    }

    pub(crate) const fn limit_reached(&self) -> bool {
        self.limit_reached
    }

    pub(crate) fn clear_unadmitted(&mut self) {
        for outcome in self
            .before
            .outcomes
            .iter_mut()
            .chain(self.after.outcomes.iter_mut())
        {
            *outcome = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::SystemTime;

    use media_core::MediaTagMetadata;

    use super::*;
    use crate::{
        DiscoveryRecordKey, LocalMediaFingerprint, LocalMediaKind, ProbedLocalMedia,
        VERIFIED_RECORD_BUFFER_LIMIT,
    };

    #[test]
    fn far_verified_waits_for_near_terminal_and_is_released_once() {
        let mut frontier = SiblingAdmissionFrontier::new_with_limits(
            0,
            2,
            AdmissionLimits {
                before: 0,
                after: 2,
                total: 2,
            },
        );
        assert!(frontier.record_terminal(AdmissionDirection::After, 1, eligible_record(1)));
        assert!(
            frontier
                .release_contiguous(VERIFIED_RECORD_BUFFER_LIMIT)
                .is_empty()
        );

        assert!(frontier.record_terminal(
            AdmissionDirection::After,
            0,
            TerminalCandidate::Ineligible
        ));
        let releases = frontier.release_contiguous(VERIFIED_RECORD_BUFFER_LIMIT);
        let released_keys = releases
            .iter()
            .flat_map(|release| release.records.iter().map(DiscoveryRecord::key))
            .collect::<Vec<_>>();
        assert_eq!(released_keys, vec![DiscoveryRecordKey::Batch(1)]);
        assert!(
            frontier
                .release_contiguous(VERIFIED_RECORD_BUFFER_LIMIT)
                .is_empty()
        );
    }

    #[test]
    fn unused_side_quota_transfers_only_after_contiguous_exhaustion() {
        let mut frontier = SiblingAdmissionFrontier::new_with_limits(
            2,
            5,
            AdmissionLimits {
                before: 2,
                after: 3,
                total: 5,
            },
        );
        for offset in 0..5 {
            assert!(frontier.record_terminal(
                AdmissionDirection::After,
                offset,
                eligible_record(10 + offset as u32)
            ));
        }
        let first = frontier.release_contiguous(VERIFIED_RECORD_BUFFER_LIMIT);
        assert_eq!(released_record_count(&first), 3);
        assert_eq!(
            first
                .last()
                .unwrap()
                .side_accounting
                .unwrap()
                .admitted_on_side,
            3
        );
        assert_eq!(
            first
                .last()
                .unwrap()
                .side_accounting
                .unwrap()
                .effective_side_quota,
            3
        );

        assert!(frontier.record_terminal(AdmissionDirection::Before, 0, eligible_record(1)));
        let before_hole = frontier.release_contiguous(VERIFIED_RECORD_BUFFER_LIMIT);
        assert_eq!(released_record_count(&before_hole), 1);
        assert!(!frontier.limit_reached());

        assert!(frontier.record_terminal(
            AdmissionDirection::Before,
            1,
            TerminalCandidate::Ineligible
        ));
        let after_exhaustion = frontier.release_contiguous(VERIFIED_RECORD_BUFFER_LIMIT);
        assert_eq!(released_record_count(&after_exhaustion), 1);
        assert!(frontier.limit_reached());
        assert!(
            frontier
                .release_contiguous(VERIFIED_RECORD_BUFFER_LIMIT)
                .is_empty()
        );
    }

    #[test]
    fn production_d43_quotas_cover_start_middle_end_and_exact_cap() {
        assert_eq!(
            AUTOMATIC_SIBLING_BEFORE_QUOTA + AUTOMATIC_SIBLING_AFTER_QUOTA,
            AUTOMATIC_SIBLING_RECORD_LIMIT
        );
        assert_eq!(target_centered_counts(0, 80_000), (0, 49_999));
        assert_eq!(target_centered_counts(80_000, 0), (49_999, 0));
        assert_eq!(target_centered_counts(80_000, 80_000), (24_999, 25_000));
        assert_eq!(target_centered_counts(12_000, 37_999), (12_000, 37_999));
        assert_eq!(target_centered_counts(12_000, 38_000), (12_000, 37_999));
    }

    #[test]
    fn cap_and_side_counts_are_invariant_to_completion_and_flush_interleaving() {
        let before_then_after = [
            (AdmissionDirection::Before, 0),
            (AdmissionDirection::Before, 1),
            (AdmissionDirection::Before, 2),
            (AdmissionDirection::Before, 3),
            (AdmissionDirection::After, 0),
            (AdmissionDirection::After, 1),
            (AdmissionDirection::After, 2),
            (AdmissionDirection::After, 3),
        ];
        let shuffled_both_sides = [
            (AdmissionDirection::After, 3),
            (AdmissionDirection::Before, 3),
            (AdmissionDirection::After, 1),
            (AdmissionDirection::Before, 1),
            (AdmissionDirection::After, 0),
            (AdmissionDirection::Before, 2),
            (AdmissionDirection::After, 2),
            (AdmissionDirection::Before, 0),
        ];

        let sequential = run_interleaving(&before_then_after, true);
        let batched_shuffled = run_interleaving(&shuffled_both_sides, false);
        assert_eq!(sequential, batched_shuffled);
        assert_eq!(sequential.before_admitted, 2);
        assert_eq!(sequential.after_admitted, 3);
        assert_eq!(sequential.total_admitted, 5);
        assert_eq!(sequential.keys.len(), 5);
        assert_eq!(
            sequential.keys,
            BTreeSet::from([
                DiscoveryRecordKey::Batch(10),
                DiscoveryRecordKey::Batch(12),
                DiscoveryRecordKey::Batch(20),
                DiscoveryRecordKey::Batch(21),
                DiscoveryRecordKey::Batch(23),
            ])
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct InterleavingResult {
        keys: BTreeSet<DiscoveryRecordKey>,
        before_admitted: usize,
        after_admitted: usize,
        total_admitted: usize,
    }

    fn run_interleaving(
        completion_order: &[(AdmissionDirection, usize)],
        flush_after_every_completion: bool,
    ) -> InterleavingResult {
        let mut frontier = SiblingAdmissionFrontier::new_with_limits(
            4,
            4,
            AdmissionLimits {
                before: 2,
                after: 3,
                total: 5,
            },
        );
        let mut keys = BTreeSet::new();
        for (completion_index, (direction, offset)) in completion_order.iter().copied().enumerate()
        {
            assert!(frontier.record_terminal(
                direction,
                offset,
                interleaving_outcome(direction, offset),
            ));
            if flush_after_every_completion || completion_index % 2 == 1 {
                collect_release_keys(&mut frontier, &mut keys);
            }
        }
        collect_release_keys(&mut frontier, &mut keys);
        assert!(frontier.limit_reached());
        InterleavingResult {
            keys,
            before_admitted: frontier.before.admitted,
            after_admitted: frontier.after.admitted,
            total_admitted: frontier.total_admitted,
        }
    }

    fn interleaving_outcome(direction: AdmissionDirection, offset: usize) -> TerminalCandidate {
        let is_failure = matches!(
            (direction, offset),
            (AdmissionDirection::Before, 1) | (AdmissionDirection::After, 2)
        );
        if is_failure {
            TerminalCandidate::Ineligible
        } else {
            let key = match direction {
                AdmissionDirection::Before => 10 + offset as u32,
                AdmissionDirection::After => 20 + offset as u32,
                AdmissionDirection::NonDirectional => unreachable!(),
            };
            eligible_record(key)
        }
    }

    fn collect_release_keys(
        frontier: &mut SiblingAdmissionFrontier,
        keys: &mut BTreeSet<DiscoveryRecordKey>,
    ) {
        for record in frontier
            .release_contiguous(VERIFIED_RECORD_BUFFER_LIMIT)
            .into_iter()
            .flat_map(|release| release.records)
        {
            assert!(keys.insert(record.key()), "record released more than once");
        }
    }

    fn target_centered_counts(before_available: usize, after_available: usize) -> (usize, usize) {
        let mut before = before_available.min(AUTOMATIC_SIBLING_BEFORE_QUOTA);
        let mut after = after_available.min(AUTOMATIC_SIBLING_AFTER_QUOTA);
        if before_available < AUTOMATIC_SIBLING_BEFORE_QUOTA {
            after = after_available.min(AUTOMATIC_SIBLING_RECORD_LIMIT - before);
        }
        if after_available < AUTOMATIC_SIBLING_AFTER_QUOTA {
            before = before_available.min(AUTOMATIC_SIBLING_RECORD_LIMIT - after);
        }
        (before, after)
    }

    fn eligible_record(index: u32) -> TerminalCandidate {
        TerminalCandidate::Eligible(Box::new(DiscoveryRecord::new(
            DiscoveryRecordKey::Batch(index),
            format!("record-{index}").into(),
            ProbedLocalMedia::new(
                format!("record-{index}"),
                LocalMediaKind::VideoContaining,
                None,
                MediaTagMetadata::default(),
                LocalMediaFingerprint::new(1, SystemTime::UNIX_EPOCH),
            ),
        )))
    }

    fn released_record_count(releases: &[FrontierRelease]) -> usize {
        releases.iter().map(|release| release.records.len()).sum()
    }
}
