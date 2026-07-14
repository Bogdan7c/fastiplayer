//! Bounded ACK state только для batches, от которых зависит D74 readiness.

use crate::{AdmissionBatchId, AdmissionDirection, ManifestCandidateKey};

const MAX_PENDING_READINESS_ACKS: usize = 2;

#[derive(Default)]
pub(crate) struct PendingReadinessAcks {
    entries: [Option<PendingReadinessAck>; MAX_PENDING_READINESS_ACKS],
}

struct PendingReadinessAck {
    batch_id: AdmissionBatchId,
    nearest_candidates: Vec<(AdmissionDirection, ManifestCandidateKey, u64)>,
}

impl PendingReadinessAcks {
    pub(crate) fn retain_if_required(
        &mut self,
        batch_id: AdmissionBatchId,
        nearest_candidates: Vec<(AdmissionDirection, ManifestCandidateKey, u64)>,
    ) {
        if nearest_candidates.is_empty() {
            return;
        }
        let vacant_slot = self
            .entries
            .iter_mut()
            .find(|entry| entry.is_none())
            .expect("Before/After readiness slots are bounded by direction");
        *vacant_slot = Some(PendingReadinessAck {
            batch_id,
            nearest_candidates,
        });
    }

    pub(crate) fn take(
        &mut self,
        batch_id: AdmissionBatchId,
    ) -> Option<Vec<(AdmissionDirection, ManifestCandidateKey, u64)>> {
        self.entries
            .iter_mut()
            .find(|entry| {
                entry
                    .as_ref()
                    .is_some_and(|pending| pending.batch_id == batch_id)
            })
            .and_then(Option::take)
            .map(|pending| pending.nearest_candidates)
    }

    pub(crate) fn clear(&mut self) {
        self.entries = std::array::from_fn(|_| None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_hundred_thousand_non_readiness_batches_retain_no_ack_state() {
        let mut pending_acks = PendingReadinessAcks::default();
        for counter in 1..=100_000 {
            pending_acks
                .retain_if_required(AdmissionBatchId::from_counter(counter).unwrap(), Vec::new());
        }
        assert!(pending_acks.entries.iter().all(Option::is_none));
        assert_eq!(MAX_PENDING_READINESS_ACKS, 2);
    }
}
