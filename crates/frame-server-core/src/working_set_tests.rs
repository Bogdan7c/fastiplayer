use std::sync::{Arc, Mutex};
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use media_core::{TimeBase, TrackDuration, TrackId, TrackTimestamp};
use video_core::{DecodedFrame, FrameMemoryPath, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_present_core::{
    SharedVideoFrameReleaseSink, VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease,
    VideoFrameReleaseOutcome, VideoFrameReleaseSink, VideoPresentFrameResourceKind,
};

use crate::*;

fn generation_token(playback: u64, scrub: u64) -> ScrubGenerationToken {
    ScrubGenerationToken::new(
        PlaybackGeneration::new(playback),
        ScrubGeneration::new(scrub),
    )
}

fn capacity(value: usize) -> std::num::NonZeroUsize {
    std::num::NonZeroUsize::new(value).expect("test capacity must be non-zero")
}

fn video_track() -> TrackId {
    TrackId::new(7)
}

fn time_base() -> TimeBase {
    TimeBase::new(1, 1_000).expect("test timebase is valid")
}

fn timestamp(millis: i64) -> TrackTimestamp {
    TrackTimestamp::new(video_track(), millis, time_base())
}

fn duration(millis: u64) -> TrackDuration {
    TrackDuration::new(video_track(), millis, time_base())
}

fn base_key(bucket: i64) -> TimelineHoverPrepareFrameKey {
    TimelineHoverPrepareFrameKey::new(
        SourceRevision::new(10),
        ScrubTrackSelection::with_audio(video_track(), TrackId::new(8)),
        BackendRevision::new(20),
        generation_token(30, 40),
        FrameExactnessPolicy::TargetOrAfter,
        TimelineHoverFrameBucket::new(bucket),
    )
}

fn lookup_request(
    key: TimelineHoverPrepareFrameKey,
    target_millis: i64,
) -> TimelineHoverPrepareFrameLookupRequest {
    TimelineHoverPrepareFrameLookupRequest::new(key, timestamp(target_millis))
}

fn timing(actual_millis: i64) -> TimelineHoverPreparedFrameTiming {
    TimelineHoverPreparedFrameTiming::new(timestamp(actual_millis))
}

fn decoded_frame(
    resource_handle: FrameResourceHandle,
    frame_contract: VideoFrameContract,
) -> DecodedFrame {
    DecodedFrame {
        generation: 30,
        pts: Duration::from_millis(1_250),
        frame_contract,
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

fn release_sink_with_storage(
    released: Arc<Mutex<Vec<FrameResourceHandle>>>,
) -> SharedVideoFrameReleaseSink {
    Arc::new(RecordingReleaseSink { released })
}

fn lease_for_contract(
    resource_handle: FrameResourceHandle,
    frame_contract: VideoFrameContract,
    released: Arc<Mutex<Vec<FrameResourceHandle>>>,
) -> VideoFrameLease {
    VideoFrameLease::new(VideoFrameLeaseConfig::new(
        90,
        decoded_frame(resource_handle, frame_contract),
        release_sink_with_storage(released),
    ))
}

fn hardware_lease(
    resource_handle: FrameResourceHandle,
    released: Arc<Mutex<Vec<FrameResourceHandle>>>,
) -> VideoFrameLease {
    lease_for_contract(
        resource_handle,
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        released,
    )
}

fn software_lease(
    resource_handle: FrameResourceHandle,
    released: Arc<Mutex<Vec<FrameResourceHandle>>>,
) -> VideoFrameLease {
    lease_for_contract(
        resource_handle,
        VideoFrameContract::host_yuv420_planar8(),
        released,
    )
}

fn entry(
    resource_handle: FrameResourceHandle,
    actual_millis: i64,
) -> TimelineHoverPreparedFrameEntry {
    TimelineHoverPreparedFrameEntry::new(
        hardware_lease(resource_handle, Arc::new(Mutex::new(Vec::new()))),
        timing(actual_millis),
    )
}

#[derive(Default)]
struct RecordingReleaseSink {
    released: Arc<Mutex<Vec<FrameResourceHandle>>>,
}

impl VideoFrameReleaseSink for RecordingReleaseSink {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        self.released
            .lock()
            .expect("release storage mutex must not be poisoned")
            .push(release.resource_handle());
        VideoFrameReleaseOutcome::Accepted
    }
}

fn released_handles(released: &Arc<Mutex<Vec<FrameResourceHandle>>>) -> Vec<FrameResourceHandle> {
    released
        .lock()
        .expect("release storage mutex must not be poisoned")
        .clone()
}

fn release_count(
    released: &Arc<Mutex<Vec<FrameResourceHandle>>>,
    resource_handle: FrameResourceHandle,
) -> usize {
    released_handles(released)
        .iter()
        .filter(|released_handle| **released_handle == resource_handle)
        .count()
}

fn recent_budget_for_tests(
    general_slots: u8,
    software_slots: u8,
) -> TimelineHoverRecentSupersededBudget {
    let validated_config = FrameServerConfig {
        recent_superseded_prepare_slots: general_slots,
        software_recent_superseded_prepare_slots: software_slots,
        ..FrameServerConfig::default()
    }
    .validate()
    .expect("test recent-superseded budget must pass config validation");

    TimelineHoverRecentSupersededBudget::from_validated_config(validated_config)
}

fn working_set_with_recent(
    general_slots: u8,
    software_slots: u8,
) -> TimelineHoverPrepareWorkingSet<FakeBranchToken> {
    TimelineHoverPrepareWorkingSet::with_capacity_and_recent_superseded(
        capacity(1),
        recent_budget_for_tests(general_slots, software_slots),
    )
}

fn admission_request(
    prepared_key: TimelineHoverPrepareFrameKey,
    protected_key: TimelineHoverPrepareFrameKey,
    mode: TimelineHoverPrepareAdmissionMode,
    provider_budget: TimelineHoverPrepareProviderBudget,
) -> TimelineHoverPrepareAdmissionRequest {
    TimelineHoverPrepareAdmissionRequest::new(prepared_key, protected_key, mode, provider_budget)
}

#[derive(Debug)]
struct FakeBranchToken {
    branch_id: u64,
}

struct FakeSeekTransaction<BranchToken> {
    promoted_frame: Option<TimelineHoverPromotedPreparedFrame<BranchToken>>,
}

impl<BranchToken> FakeSeekTransaction<BranchToken> {
    fn new(promoted_frame: TimelineHoverPromotedPreparedFrame<BranchToken>) -> Self {
        Self {
            promoted_frame: Some(promoted_frame),
        }
    }

    fn promoted_frame(&self) -> &TimelineHoverPromotedPreparedFrame<BranchToken> {
        self.promoted_frame
            .as_ref()
            .expect("fake transaction must own a promoted frame until finish")
    }

    fn commit(mut self) {
        drop(self.promoted_frame.take());
    }

    fn cancel(mut self) {
        drop(self.promoted_frame.take());
    }

    fn audio_failure(mut self) {
        drop(self.promoted_frame.take());
    }

    fn supersede_to_recent(
        mut self,
        working_set: &mut TimelineHoverPrepareWorkingSet<BranchToken>,
        request: TimelineHoverPrepareFrameLookupRequest,
    ) -> TimelineHoverPrepareDemoteBackOutcome<BranchToken> {
        let promoted_frame = self
            .promoted_frame
            .take()
            .expect("fake transaction must own promoted frame before supersede");

        working_set.try_demote_promoted_frame_to_recent_superseded(
            promoted_frame,
            request,
            CancelScrubReason::SupersededByNewTarget,
        )
    }
}

#[derive(Clone, Copy)]
enum FakeSeekFinishReason {
    Commit,
    Cancel,
    AudioFailure,
}

impl FakeSeekFinishReason {
    fn finish_transaction<BranchToken>(self, transaction: FakeSeekTransaction<BranchToken>) {
        match self {
            Self::Commit => transaction.commit(),
            Self::Cancel => transaction.cancel(),
            Self::AudioFailure => transaction.audio_failure(),
        }
    }
}

fn promote_branch_entry(
    working_set: &mut TimelineHoverPrepareWorkingSet<FakeBranchToken>,
    key: TimelineHoverPrepareFrameKey,
    resource_handle: FrameResourceHandle,
    released: Arc<Mutex<Vec<FrameResourceHandle>>>,
) -> FakeSeekTransaction<FakeBranchToken> {
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(resource_handle, released),
            timing(1_250),
        )
        .with_branch_token(FakeBranchToken { branch_id: 0 }),
    );

    let promotion = working_set.promote_prepared_frame(lookup_request(key, 1_200));
    let TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(promoted_frame) = promotion
    else {
        panic!("validated branch entry must promote into fake seek transaction");
    };

    FakeSeekTransaction::new(promoted_frame)
}

#[test]
fn lookup_hits_for_same_source_track_policy_backend_and_hover_generation() {
    let key = base_key(12);
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(2));
    working_set.insert_prepared_frame(key, entry(FrameResourceHandle(1), 1_250));

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 1_200));

    let TimelineHoverPrepareLookupOutcome::Hit(frame) = lookup else {
        panic!("same guards and TargetOrAfter timing must hit");
    };
    assert_eq!(frame.key(), key);
    assert_eq!(frame.timing().actual_pts(), timestamp(1_250));
}

#[test]
fn stale_source_revision_invalidates_prepared_entry() {
    let stored_key = base_key(12);
    let requested_key = TimelineHoverPrepareFrameKey::new(
        SourceRevision::new(11),
        stored_key.track_selection(),
        stored_key.backend_revision(),
        stored_key.hover_generation(),
        stored_key.exactness_policy(),
        stored_key.target_bucket(),
    );
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(2));
    working_set.insert_prepared_frame(stored_key, entry(FrameResourceHandle(2), 1_250));

    let lookup = working_set.lookup_prepared_frame(lookup_request(requested_key, 1_200));

    let TimelineHoverPrepareLookupOutcome::Miss(
        TimelineHoverPrepareLookupMissReason::SourceRevisionMismatch { stored, requested },
    ) = lookup
    else {
        panic!("source revision mismatch must be a typed miss");
    };
    assert_eq!(stored, SourceRevision::new(10));
    assert_eq!(requested, SourceRevision::new(11));
}

#[test]
fn stale_backend_revision_invalidates_prepared_entry() {
    let stored_key = base_key(12);
    let requested_key = TimelineHoverPrepareFrameKey::new(
        stored_key.source_revision(),
        stored_key.track_selection(),
        BackendRevision::new(21),
        stored_key.hover_generation(),
        stored_key.exactness_policy(),
        stored_key.target_bucket(),
    );
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(2));
    working_set.insert_prepared_frame(stored_key, entry(FrameResourceHandle(3), 1_250));

    let lookup = working_set.lookup_prepared_frame(lookup_request(requested_key, 1_200));

    let TimelineHoverPrepareLookupOutcome::Miss(
        TimelineHoverPrepareLookupMissReason::BackendRevisionMismatch { stored, requested },
    ) = lookup
    else {
        panic!("backend revision mismatch must be a typed miss");
    };
    assert_eq!(stored, BackendRevision::new(20));
    assert_eq!(requested, BackendRevision::new(21));
}

#[test]
fn stale_hover_generation_invalidates_prepared_entry() {
    let stored_key = base_key(12);
    let requested_key = TimelineHoverPrepareFrameKey::new(
        stored_key.source_revision(),
        stored_key.track_selection(),
        stored_key.backend_revision(),
        generation_token(30, 41),
        stored_key.exactness_policy(),
        stored_key.target_bucket(),
    );
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(2));
    working_set.insert_prepared_frame(stored_key, entry(FrameResourceHandle(4), 1_250));

    let lookup = working_set.lookup_prepared_frame(lookup_request(requested_key, 1_200));

    assert!(matches!(
        lookup,
        TimelineHoverPrepareLookupOutcome::Miss(
            TimelineHoverPrepareLookupMissReason::HoverGenerationMismatch {
                stored,
                requested,
            }
        ) if stored == generation_token(30, 40) && requested == generation_token(30, 41)
    ));
}

#[test]
fn bucket_collision_rejects_actual_pts_before_target_for_target_or_after() {
    let key = base_key(12);
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(2));
    working_set.insert_prepared_frame(key, entry(FrameResourceHandle(5), 900));

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 950));

    assert!(matches!(
        lookup,
        TimelineHoverPrepareLookupOutcome::TimingRejected(
            TimelineHoverPrepareTimingRejection::ActualFrameBeforeRequestedTarget {
                actual_pts,
                requested_target_pts,
            }
        ) if actual_pts == timestamp(900) && requested_target_pts == timestamp(950)
    ));
}

#[test]
fn estimated_duration_does_not_prove_exactness_for_irregular_timing() {
    let key = base_key(12);
    let optimistic_timing = TimelineHoverPreparedFrameTiming::new(timestamp(900))
        .with_estimated_duration(duration(200));
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(2));
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(6), Arc::new(Mutex::new(Vec::new()))),
            optimistic_timing,
        ),
    );

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 1_000));

    assert!(matches!(
        lookup,
        TimelineHoverPrepareLookupOutcome::TimingRejected(
            TimelineHoverPrepareTimingRejection::ActualFrameBeforeRequestedTarget { .. }
        )
    ));
}

#[test]
fn hardware_style_entry_stores_lease_resource_metadata_not_pixel_bytes() {
    let key = base_key(12);
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(2));
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(7), Arc::new(Mutex::new(Vec::new()))),
            timing(1_250),
        ),
    );

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 1_200));

    let TimelineHoverPrepareLookupOutcome::Hit(frame) = lookup else {
        panic!("hardware-style metadata entry must hit");
    };
    assert_eq!(
        frame.resource_descriptor().kind(),
        VideoPresentFrameResourceKind::DmaBufZeroCopy
    );
    assert_eq!(
        frame.resource_descriptor().resource_handle(),
        FrameResourceHandle(7)
    );
    assert_eq!(
        frame.resource_descriptor().memory_path(),
        FrameMemoryPath::DmaBufZeroCopy
    );
}

#[test]
fn software_style_entry_stores_lease_resource_metadata_not_pixel_bytes() {
    let key = base_key(12);
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(2));
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::new(
            software_lease(FrameResourceHandle(8), Arc::new(Mutex::new(Vec::new()))),
            timing(1_250),
        ),
    );

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 1_200));

    let TimelineHoverPrepareLookupOutcome::Hit(frame) = lookup else {
        panic!("software-style metadata entry must hit");
    };
    assert_eq!(
        frame.resource_descriptor().kind(),
        VideoPresentFrameResourceKind::HostPlanar
    );
    assert_eq!(
        frame.resource_descriptor().resource_handle(),
        FrameResourceHandle(8)
    );
    assert_eq!(
        frame.resource_descriptor().memory_path(),
        FrameMemoryPath::CpuUpload
    );
}

#[test]
fn optional_branch_token_is_opaque_and_returned_by_identity() {
    #[derive(Debug)]
    struct OpaqueBranchToken {
        _private_marker: (),
    }

    let key = base_key(12);
    let branch_token = Arc::new(OpaqueBranchToken {
        _private_marker: (),
    });
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<Arc<OpaqueBranchToken>>::with_capacity(capacity(2));
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::<Arc<OpaqueBranchToken>>::new(
            hardware_lease(FrameResourceHandle(9), Arc::new(Mutex::new(Vec::new()))),
            timing(1_250),
        )
        .with_branch_token(branch_token.clone()),
    );

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 1_200));

    let TimelineHoverPrepareLookupOutcome::Hit(frame) = lookup else {
        panic!("entry with opaque branch token must hit");
    };
    let returned_token = frame
        .branch_token()
        .expect("branch token must be returned when stored");
    assert!(Arc::ptr_eq(returned_token, &branch_token));
}

#[test]
fn eviction_releases_evicted_lease_exactly_once() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let mut working_set = TimelineHoverPrepareWorkingSet::new(capacity(1));
    working_set.insert_prepared_frame(
        base_key(1),
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(10), released.clone()),
            timing(1_250),
        ),
    );
    working_set.insert_prepared_frame(
        base_key(2),
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(11), released.clone()),
            timing(1_260),
        ),
    );

    let releases_after_eviction = released
        .lock()
        .expect("release storage mutex must not be poisoned")
        .clone();
    assert_eq!(releases_after_eviction, vec![FrameResourceHandle(10)]);

    drop(working_set);

    let releases_after_drop = released
        .lock()
        .expect("release storage mutex must not be poisoned")
        .clone();
    assert_eq!(
        releases_after_drop
            .iter()
            .filter(|handle| **handle == FrameResourceHandle(10))
            .count(),
        1
    );
    assert_eq!(
        releases_after_drop
            .iter()
            .filter(|handle| **handle == FrameResourceHandle(11))
            .count(),
        1
    );
}

#[test]
fn pressure_releases_recent_before_primary_byproducts_and_protects_current_target() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let current_key = base_key(200);
    let byproduct_key = base_key(201);
    let recent_key = base_key(202);
    let mut working_set = TimelineHoverPrepareWorkingSet::with_capacity_and_recent_superseded(
        capacity(2),
        recent_budget_for_tests(1, 1),
    );
    working_set.insert_prepared_frame(
        current_key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(200), released.clone()),
            timing(1_250),
        ),
    );
    working_set.insert_prepared_frame(
        byproduct_key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(201), released.clone()),
            timing(1_260),
        ),
    );
    let mut transaction_source = working_set_with_recent(0, 0);
    let transaction = promote_branch_entry(
        &mut transaction_source,
        recent_key,
        FrameResourceHandle(202),
        released.clone(),
    );
    let demote =
        transaction.supersede_to_recent(&mut working_set, lookup_request(recent_key, 1_200));
    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));

    let recent_pressure = working_set.release_one_for_resource_pressure(current_key);

    assert_eq!(
        recent_pressure,
        TimelineHoverPreparePressureReleaseOutcome::ReleasedRecentSuperseded {
            released_key: recent_key,
        }
    );
    assert_eq!(release_count(&released, FrameResourceHandle(202)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(201)), 0);
    assert_eq!(release_count(&released, FrameResourceHandle(200)), 0);

    let primary_pressure = working_set.release_one_for_resource_pressure(current_key);

    assert_eq!(
        primary_pressure,
        TimelineHoverPreparePressureReleaseOutcome::ReleasedPrimaryByproduct {
            released_key: byproduct_key,
        }
    );
    assert_eq!(release_count(&released, FrameResourceHandle(201)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(200)), 0);

    let protected_pressure = working_set.release_one_for_resource_pressure(current_key);

    assert_eq!(
        protected_pressure,
        TimelineHoverPreparePressureReleaseOutcome::NothingReleased {
            reason: TimelineHoverPreparePressureReleaseMissReason::OnlyProtectedCurrentTarget {
                protected_key: current_key,
            },
        }
    );
    assert_eq!(release_count(&released, FrameResourceHandle(200)), 0);
}

#[test]
fn live_capacity_shrink_releases_excess_entries_by_pressure_order() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let current_key = base_key(250);
    let first_byproduct_key = base_key(251);
    let second_byproduct_key = base_key(252);
    let recent_key = base_key(253);
    let mut working_set = TimelineHoverPrepareWorkingSet::with_capacity_and_recent_superseded(
        capacity(3),
        recent_budget_for_tests(1, 1),
    );
    for (key, handle) in [
        (current_key, FrameResourceHandle(250)),
        (first_byproduct_key, FrameResourceHandle(251)),
        (second_byproduct_key, FrameResourceHandle(252)),
    ] {
        working_set.insert_prepared_frame(
            key,
            TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
                hardware_lease(handle, released.clone()),
                timing(1_250),
            ),
        );
    }
    let mut transaction_source = working_set_with_recent(0, 0);
    let transaction = promote_branch_entry(
        &mut transaction_source,
        recent_key,
        FrameResourceHandle(253),
        released.clone(),
    );
    let demote =
        transaction.supersede_to_recent(&mut working_set, lookup_request(recent_key, 1_200));
    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));

    let outcome = working_set.reconfigure_primary_capacity(capacity(1), current_key);

    assert_eq!(outcome.old_capacity(), 3);
    assert_eq!(outcome.new_capacity(), 1);
    assert_eq!(
        outcome.released_entries(),
        &[
            TimelineHoverPreparePressureReleaseOutcome::ReleasedRecentSuperseded {
                released_key: recent_key,
            },
            TimelineHoverPreparePressureReleaseOutcome::ReleasedPrimaryByproduct {
                released_key: first_byproduct_key,
            },
            TimelineHoverPreparePressureReleaseOutcome::ReleasedPrimaryByproduct {
                released_key: second_byproduct_key,
            },
        ]
    );
    assert_eq!(working_set.capacity().get(), 1);
    assert_eq!(working_set.len(), 1);
    assert!(matches!(
        working_set.lookup_prepared_frame(lookup_request(current_key, 1_200)),
        TimelineHoverPrepareLookupOutcome::Hit(_)
    ));
    assert_eq!(release_count(&released, FrameResourceHandle(250)), 0);
    assert_eq!(release_count(&released, FrameResourceHandle(251)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(252)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(253)), 1);
}

#[test]
fn live_capacity_grow_only_changes_future_admission_without_refill() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let current_key = base_key(260);
    let next_key = base_key(261);
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(1));
    working_set.insert_prepared_frame(
        current_key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(260), released.clone()),
            timing(1_250),
        ),
    );

    let outcome = working_set.reconfigure_primary_capacity(capacity(3), current_key);

    assert_eq!(outcome.old_capacity(), 1);
    assert_eq!(outcome.new_capacity(), 3);
    assert!(outcome.released_entries().is_empty());
    assert_eq!(working_set.len(), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(260)), 0);

    let admitted = working_set.evaluate_prepare_admission(admission_request(
        next_key,
        current_key,
        TimelineHoverPrepareAdmissionMode::ResumePendingAfterSeekPin,
        TimelineHoverPrepareProviderBudget::SpareSlotAvailable,
    ));
    assert_eq!(
        admitted,
        TimelineHoverPrepareAdmissionOutcome::Admitted {
            slot_plan: TimelineHoverPrepareSlotPlan::UseSparePrimarySlot,
        }
    );
}

#[test]
fn pressure_never_touches_active_seek_owned_promoted_resource() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let promoted_key = base_key(210);
    let byproduct_key = base_key(211);
    let protected_key = base_key(212);
    let mut working_set = TimelineHoverPrepareWorkingSet::with_capacity(capacity(1));
    let transaction = promote_branch_entry(
        &mut working_set,
        promoted_key,
        FrameResourceHandle(210),
        released.clone(),
    );
    working_set.insert_prepared_frame(
        byproduct_key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(211), released.clone()),
            timing(1_260),
        ),
    );

    let pressure = working_set.release_one_for_resource_pressure(protected_key);

    assert_eq!(
        pressure,
        TimelineHoverPreparePressureReleaseOutcome::ReleasedPrimaryByproduct {
            released_key: byproduct_key,
        }
    );
    assert_eq!(release_count(&released, FrameResourceHandle(211)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(210)), 0);

    transaction.commit();
    assert_eq!(release_count(&released, FrameResourceHandle(210)), 1);
}

#[test]
fn session_end_release_clears_hover_owned_entries_without_promoted_seek_resource() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let first_primary_key = base_key(230);
    let second_primary_key = base_key(231);
    let recent_key = base_key(232);
    let active_seek_key = base_key(233);
    let mut working_set = TimelineHoverPrepareWorkingSet::with_capacity_and_recent_superseded(
        capacity(3),
        recent_budget_for_tests(1, 1),
    );
    working_set.insert_prepared_frame(
        first_primary_key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(230), released.clone()),
            timing(1_250),
        ),
    );
    working_set.insert_prepared_frame(
        second_primary_key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(231), released.clone()),
            timing(1_260),
        ),
    );

    let recent_transaction = promote_branch_entry(
        &mut working_set,
        recent_key,
        FrameResourceHandle(232),
        released.clone(),
    );
    let demote =
        recent_transaction.supersede_to_recent(&mut working_set, lookup_request(recent_key, 1_200));
    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));
    let active_seek_transaction = promote_branch_entry(
        &mut working_set,
        active_seek_key,
        FrameResourceHandle(233),
        released.clone(),
    );

    let release_outcome = working_set.release_hover_owned_entries_for_session_end(
        TimelineHoverPrepareSessionEndReleaseReason::LeaveGraceExpired,
    );
    let repeated_release = working_set.release_hover_owned_entries_for_session_end(
        TimelineHoverPrepareSessionEndReleaseReason::LeaveGraceExpired,
    );

    assert_eq!(
        release_outcome,
        TimelineHoverPrepareSessionEndReleaseOutcome::new(2, 1)
    );
    assert_eq!(
        repeated_release,
        TimelineHoverPrepareSessionEndReleaseOutcome::default()
    );
    assert_eq!(working_set.len(), 0);
    assert_eq!(working_set.recent_superseded_len(), 0);
    assert_eq!(release_count(&released, FrameResourceHandle(230)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(231)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(232)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(233)), 0);

    drop(active_seek_transaction);
    assert_eq!(release_count(&released, FrameResourceHandle(233)), 1);
}

#[test]
fn primary_hover_byproducts_use_latest_n_without_evicting_current_target() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let current_key = base_key(220);
    let first_byproduct_key = base_key(221);
    let second_byproduct_key = base_key(222);
    let newest_byproduct_key = base_key(223);
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(3));

    for (key, handle) in [
        (current_key, FrameResourceHandle(220)),
        (first_byproduct_key, FrameResourceHandle(221)),
        (second_byproduct_key, FrameResourceHandle(222)),
        (newest_byproduct_key, FrameResourceHandle(223)),
    ] {
        let insert = working_set.try_insert_prepared_frame(
            admission_request(
                key,
                current_key,
                TimelineHoverPrepareAdmissionMode::NormalHover,
                TimelineHoverPrepareProviderBudget::SpareSlotAvailable,
            ),
            TimelineHoverPreparedFrameEntry::new(
                hardware_lease(handle, released.clone()),
                timing(1_250),
            ),
        );

        assert!(matches!(
            insert,
            TimelineHoverPrepareInsertOutcome::Inserted { .. }
        ));
    }

    assert_eq!(working_set.len(), 3);
    assert_eq!(release_count(&released, FrameResourceHandle(221)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(220)), 0);
    assert!(matches!(
        working_set.lookup_prepared_frame(lookup_request(current_key, 1_200)),
        TimelineHoverPrepareLookupOutcome::Hit(_)
    ));
    assert!(matches!(
        working_set.lookup_prepared_frame(lookup_request(second_byproduct_key, 1_200)),
        TimelineHoverPrepareLookupOutcome::Hit(_)
    ));
    assert!(matches!(
        working_set.lookup_prepared_frame(lookup_request(newest_byproduct_key, 1_200)),
        TimelineHoverPrepareLookupOutcome::Hit(_)
    ));
}

#[test]
fn provider_pressure_insert_noop_returns_entry_without_working_set_side_effects() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let current_key = base_key(230);
    let rejected_key = base_key(231);
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(1));
    working_set.insert_prepared_frame(
        current_key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(230), released.clone()),
            timing(1_250),
        ),
    );

    let insert = working_set.try_insert_prepared_frame(
        admission_request(
            rejected_key,
            current_key,
            TimelineHoverPrepareAdmissionMode::NormalHover,
            TimelineHoverPrepareProviderBudget::ExhaustedAfterActivePins,
        ),
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(231), released.clone()),
            timing(1_260),
        ),
    );

    let TimelineHoverPrepareInsertOutcome::NoOp { entry, reason } = insert else {
        panic!("provider pressure must reject prepare without mutating working set");
    };
    assert_eq!(
        reason,
        TimelineHoverPrepareNoOpReason::ProviderResourcePressure
    );
    assert_eq!(working_set.len(), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(230)), 0);
    assert_eq!(release_count(&released, FrameResourceHandle(231)), 0);
    assert!(matches!(
        working_set.lookup_prepared_frame(lookup_request(current_key, 1_200)),
        TimelineHoverPrepareLookupOutcome::Hit(_)
    ));

    drop(entry);
    assert_eq!(release_count(&released, FrameResourceHandle(231)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(230)), 0);
}

#[test]
fn resume_pending_hover_prepare_requires_spare_slot_after_seek_pin() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let current_key = base_key(240);
    let next_key = base_key(241);
    let mut full_working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(1));
    full_working_set.insert_prepared_frame(
        current_key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(240), released.clone()),
            timing(1_250),
        ),
    );

    let no_spare = full_working_set.evaluate_prepare_admission(admission_request(
        next_key,
        current_key,
        TimelineHoverPrepareAdmissionMode::ResumePendingAfterSeekPin,
        TimelineHoverPrepareProviderBudget::SpareSlotAvailable,
    ));

    assert_eq!(
        no_spare,
        TimelineHoverPrepareAdmissionOutcome::NoOp {
            reason: TimelineHoverPrepareNoOpReason::NoSpareHoverSlot {
                capacity: 1,
                used_slots: 1,
                protected_key: current_key,
            },
        }
    );
    assert_eq!(release_count(&released, FrameResourceHandle(240)), 0);

    let mut spare_working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(2));
    spare_working_set.insert_prepared_frame(
        current_key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(242), released.clone()),
            timing(1_250),
        ),
    );

    let admitted = spare_working_set.evaluate_prepare_admission(admission_request(
        next_key,
        current_key,
        TimelineHoverPrepareAdmissionMode::ResumePendingAfterSeekPin,
        TimelineHoverPrepareProviderBudget::SpareSlotAvailable,
    ));

    assert_eq!(
        admitted,
        TimelineHoverPrepareAdmissionOutcome::Admitted {
            slot_plan: TimelineHoverPrepareSlotPlan::UseSparePrimarySlot,
        }
    );
}

#[test]
fn promotion_requires_validation_before_removing_entry() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(50);
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(2));
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(50), released.clone()),
            timing(1_100),
        )
        .with_branch_token(FakeBranchToken { branch_id: 50 }),
    );

    let promotion = working_set.promote_prepared_frame(lookup_request(key, 1_200));

    let TimelineHoverPreparePromotionOutcome::TimingRejected(
        TimelineHoverPrepareTimingRejection::ActualFrameBeforeRequestedTarget {
            actual_pts,
            requested_target_pts,
        },
    ) = promotion
    else {
        panic!("promotion must reject a frame whose actual PTS is before requested target");
    };
    assert_eq!(actual_pts, timestamp(1_100));
    assert_eq!(requested_target_pts, timestamp(1_200));
    assert_eq!(working_set.len(), 1);
    assert!(released_handles(&released).is_empty());

    let lookup_after_rejection = working_set.lookup_prepared_frame(lookup_request(key, 1_000));
    let TimelineHoverPrepareLookupOutcome::Hit(frame_after_rejection) = lookup_after_rejection
    else {
        panic!("timing-rejected promotion must not remove the hover-owned entry");
    };
    assert_eq!(
        frame_after_rejection
            .branch_token()
            .expect("branch token must stay in hover entry after rejected promotion")
            .branch_id,
        50
    );

    drop(working_set);
    assert_eq!(released_handles(&released), vec![FrameResourceHandle(50)]);
}

#[test]
fn promotion_miss_keeps_hover_owned_entry_in_place() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let stored_key = base_key(55);
    let requested_key = TimelineHoverPrepareFrameKey::new(
        SourceRevision::new(11),
        stored_key.track_selection(),
        stored_key.backend_revision(),
        stored_key.hover_generation(),
        stored_key.exactness_policy(),
        stored_key.target_bucket(),
    );
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(1));
    working_set.insert_prepared_frame(
        stored_key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(55), released.clone()),
            timing(1_250),
        )
        .with_branch_token(FakeBranchToken { branch_id: 55 }),
    );

    let promotion = working_set.promote_prepared_frame(lookup_request(requested_key, 1_200));

    let TimelineHoverPreparePromotionOutcome::Miss(
        TimelineHoverPrepareLookupMissReason::SourceRevisionMismatch { stored, requested },
    ) = promotion
    else {
        panic!("promotion must keep source-revision mismatch as a typed miss");
    };
    assert_eq!(stored, SourceRevision::new(10));
    assert_eq!(requested, SourceRevision::new(11));
    assert_eq!(working_set.len(), 1);
    assert!(released_handles(&released).is_empty());

    let lookup = working_set.lookup_prepared_frame(lookup_request(stored_key, 1_200));
    let TimelineHoverPrepareLookupOutcome::Hit(frame) = lookup else {
        panic!("missed promotion must not remove the stored hover entry");
    };
    assert_eq!(
        frame
            .branch_token()
            .expect("branch token must remain hover-owned after miss")
            .branch_id,
        55
    );

    drop(working_set);
    assert_eq!(released_handles(&released), vec![FrameResourceHandle(55)]);
}

#[test]
fn promoted_branch_entry_leaves_hover_eviction_and_drop_cleanup_domain() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let promoted_key = base_key(60);
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(1));
    working_set.insert_prepared_frame(
        promoted_key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(60), released.clone()),
            timing(1_250),
        )
        .with_branch_token(FakeBranchToken { branch_id: 60 }),
    );

    let promotion = working_set.promote_prepared_frame(lookup_request(promoted_key, 1_200));
    let TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(promoted_frame) = promotion
    else {
        panic!("validated branch entry must promote as resume-ready");
    };
    let transaction = FakeSeekTransaction::new(promoted_frame);
    let TimelineHoverPromotedFrameSeekReuse::ResumeReadyBranch { branch_token } =
        transaction.promoted_frame().seek_reuse()
    else {
        panic!("branch token must make promoted entry resume-ready");
    };
    assert_eq!(branch_token.branch_id, 60);
    assert_eq!(working_set.len(), 0);

    working_set.insert_prepared_frame(
        base_key(61),
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(61), released.clone()),
            timing(1_260),
        ),
    );
    working_set.insert_prepared_frame(
        base_key(62),
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(62), released.clone()),
            timing(1_270),
        ),
    );

    assert_eq!(release_count(&released, FrameResourceHandle(61)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(60)), 0);

    drop(working_set);
    assert_eq!(release_count(&released, FrameResourceHandle(62)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(60)), 0);

    transaction.commit();
    assert_eq!(release_count(&released, FrameResourceHandle(60)), 1);
}

#[test]
fn preview_borrow_and_seek_promotion_share_same_provider_resource() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(70);
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<Arc<FakeBranchToken>>::with_capacity(capacity(2));
    let branch_token = Arc::new(FakeBranchToken { branch_id: 70 });
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(70), released.clone()),
            timing(1_250),
        )
        .with_branch_token(branch_token.clone()),
    );

    let preview_lease = {
        let lookup = working_set.lookup_prepared_frame(lookup_request(key, 1_200));
        let TimelineHoverPrepareLookupOutcome::Hit(preview_frame) = lookup else {
            panic!("preview lookup must hit before promotion");
        };
        assert!(Arc::ptr_eq(
            preview_frame
                .branch_token()
                .expect("preview must borrow stored branch token"),
            &branch_token
        ));
        preview_frame.lease().clone()
    };
    let preview_descriptor = preview_lease.resource_descriptor();

    let promotion = working_set.promote_prepared_frame(lookup_request(key, 1_200));
    let TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(promoted_frame) = promotion
    else {
        panic!("validated preview entry with branch token must promote for seek resume");
    };
    assert_eq!(promoted_frame.resource_descriptor(), preview_descriptor);
    let transaction = FakeSeekTransaction::new(promoted_frame);

    drop(working_set);
    assert!(released_handles(&released).is_empty());

    drop(preview_lease);
    assert!(released_handles(&released).is_empty());

    transaction.commit();
    assert_eq!(released_handles(&released), vec![FrameResourceHandle(70)]);
}

#[test]
fn frame_only_promotion_is_visual_override_not_resume_ready_branch() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(80);
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(1));
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(80), released.clone()),
            timing(1_250),
        ),
    );

    let promotion = working_set.promote_prepared_frame(lookup_request(key, 1_200));
    let TimelineHoverPreparePromotionOutcome::PromotedVisualOverrideResumePending(promoted_frame) =
        promotion
    else {
        panic!("frame-only entry can only promote as visual override/resume_pending input");
    };
    assert!(promoted_frame.branch_token().is_none());
    assert!(matches!(
        promoted_frame.seek_reuse(),
        TimelineHoverPromotedFrameSeekReuse::VisualOverrideResumePending
    ));

    FakeSeekTransaction::new(promoted_frame).commit();
    assert_eq!(released_handles(&released), vec![FrameResourceHandle(80)]);
}

fn promoted_transaction_releases_once_after_finish(finish_reason: FakeSeekFinishReason) {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(90);
    let mut working_set =
        TimelineHoverPrepareWorkingSet::<FakeBranchToken>::with_capacity(capacity(1));
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(90), released.clone()),
            timing(1_250),
        )
        .with_branch_token(FakeBranchToken { branch_id: 90 }),
    );

    let promotion = working_set.promote_prepared_frame(lookup_request(key, 1_200));
    let TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(promoted_frame) = promotion
    else {
        panic!("validated branch entry must promote into fake seek transaction");
    };
    let transaction = FakeSeekTransaction::new(promoted_frame);

    drop(working_set);
    assert!(released_handles(&released).is_empty());

    finish_reason.finish_transaction(transaction);
    assert_eq!(release_count(&released, FrameResourceHandle(90)), 1);
}

#[test]
fn fake_transaction_commit_releases_promoted_entry_once() {
    promoted_transaction_releases_once_after_finish(FakeSeekFinishReason::Commit);
}

#[test]
fn fake_transaction_cancel_releases_promoted_entry_once() {
    promoted_transaction_releases_once_after_finish(FakeSeekFinishReason::Cancel);
}

#[test]
fn fake_transaction_audio_failure_releases_promoted_entry_once() {
    promoted_transaction_releases_once_after_finish(FakeSeekFinishReason::AudioFailure);
}

#[test]
fn demote_back_only_accepts_superseded_by_new_target() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(100);
    let mut working_set = working_set_with_recent(1, 1);
    let transaction = promote_branch_entry(
        &mut working_set,
        key,
        FrameResourceHandle(100),
        released.clone(),
    );
    let promoted_frame = transaction
        .promoted_frame
        .expect("fake transaction must own promoted frame before explicit demote test");

    let demote = working_set.try_demote_promoted_frame_to_recent_superseded(
        promoted_frame,
        lookup_request(key, 1_200),
        CancelScrubReason::UserCancelled,
    );

    let TimelineHoverPrepareDemoteBackOutcome::Rejected {
        promoted_frame,
        reason: TimelineHoverPrepareDemoteBackRejection::CancelReasonDoesNotAllowDemote { actual },
    } = demote
    else {
        panic!("user cancel must bypass recent_superseded demote");
    };
    assert_eq!(actual, CancelScrubReason::UserCancelled);
    assert_eq!(working_set.recent_superseded_len(), 0);
    assert!(released_handles(&released).is_empty());

    drop(promoted_frame);
    assert_eq!(released_handles(&released), vec![FrameResourceHandle(100)]);
}

#[test]
fn commit_cancel_and_audio_failure_bypass_recent_demote() {
    for (finish_reason, bucket, resource_handle) in [
        (FakeSeekFinishReason::Commit, 101, FrameResourceHandle(101)),
        (FakeSeekFinishReason::Cancel, 102, FrameResourceHandle(102)),
        (
            FakeSeekFinishReason::AudioFailure,
            103,
            FrameResourceHandle(103),
        ),
    ] {
        let released = Arc::new(Mutex::new(Vec::new()));
        let key = base_key(bucket);
        let mut working_set = working_set_with_recent(1, 1);
        let transaction =
            promote_branch_entry(&mut working_set, key, resource_handle, released.clone());

        finish_reason.finish_transaction(transaction);

        assert_eq!(working_set.recent_superseded_len(), 0);
        assert_eq!(released_handles(&released), vec![resource_handle]);
    }
}

#[test]
fn superseded_demote_feeds_click_back_promotion_without_second_decode() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(110);
    let mut working_set = working_set_with_recent(1, 1);
    let transaction = promote_branch_entry(
        &mut working_set,
        key,
        FrameResourceHandle(110),
        released.clone(),
    );

    let demote = transaction.supersede_to_recent(&mut working_set, lookup_request(key, 1_200));

    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));
    assert_eq!(working_set.len(), 0);
    assert_eq!(working_set.recent_superseded_len(), 1);
    assert!(released_handles(&released).is_empty());

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 1_200));
    let TimelineHoverPrepareLookupOutcome::Hit(frame) = lookup else {
        panic!("demoted recent entry must be available for click-back lookup");
    };
    assert_eq!(
        frame.resource_descriptor().resource_handle(),
        FrameResourceHandle(110)
    );

    let promotion = working_set.promote_prepared_frame(lookup_request(key, 1_200));
    let TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(promoted_frame) = promotion
    else {
        panic!("recent click-back entry must promote without a second decode");
    };
    assert_eq!(working_set.recent_superseded_len(), 0);

    FakeSeekTransaction::new(promoted_frame).commit();
    assert_eq!(release_count(&released, FrameResourceHandle(110)), 1);
}

#[test]
fn zero_recent_slots_release_through_transaction_for_matching_path() {
    let hardware_released = Arc::new(Mutex::new(Vec::new()));
    let hardware_key = base_key(120);
    let mut hardware_working_set = working_set_with_recent(0, 1);
    let hardware_transaction = promote_branch_entry(
        &mut hardware_working_set,
        hardware_key,
        FrameResourceHandle(120),
        hardware_released.clone(),
    );

    let hardware_demote = hardware_transaction.supersede_to_recent(
        &mut hardware_working_set,
        lookup_request(hardware_key, 1_200),
    );
    let TimelineHoverPrepareDemoteBackOutcome::Rejected {
        promoted_frame: hardware_frame,
        reason:
            TimelineHoverPrepareDemoteBackRejection::RecentSupersededRetentionDisabled { resource_kind },
    } = hardware_demote
    else {
        panic!("zero general slots must reject hardware click-back retention");
    };
    assert_eq!(resource_kind, VideoPresentFrameResourceKind::DmaBufZeroCopy);
    drop(hardware_frame);
    assert_eq!(
        released_handles(&hardware_released),
        vec![FrameResourceHandle(120)]
    );

    let software_released = Arc::new(Mutex::new(Vec::new()));
    let software_key = base_key(121);
    let mut software_working_set = working_set_with_recent(1, 0);
    software_working_set.insert_prepared_frame(
        software_key,
        TimelineHoverPreparedFrameEntry::new(
            software_lease(FrameResourceHandle(121), software_released.clone()),
            timing(1_250),
        )
        .with_branch_token(FakeBranchToken { branch_id: 121 }),
    );
    let promotion =
        software_working_set.promote_prepared_frame(lookup_request(software_key, 1_200));
    let TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(software_frame) = promotion
    else {
        panic!("software branch entry must promote before zero-slot demote check");
    };

    let software_demote = software_working_set.try_demote_promoted_frame_to_recent_superseded(
        software_frame,
        lookup_request(software_key, 1_200),
        CancelScrubReason::SupersededByNewTarget,
    );
    let TimelineHoverPrepareDemoteBackOutcome::Rejected {
        promoted_frame: software_frame,
        reason:
            TimelineHoverPrepareDemoteBackRejection::RecentSupersededRetentionDisabled { resource_kind },
    } = software_demote
    else {
        panic!("zero software slots must reject software click-back retention");
    };
    assert_eq!(resource_kind, VideoPresentFrameResourceKind::HostPlanar);
    drop(software_frame);
    assert_eq!(
        released_handles(&software_released),
        vec![FrameResourceHandle(121)]
    );
}

#[test]
fn latest_n_recent_superseded_evicts_oldest_demoted_entry() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let mut working_set = TimelineHoverPrepareWorkingSet::with_capacity_and_recent_superseded(
        capacity(1),
        recent_budget_for_tests(2, 1),
    );

    for (bucket, handle) in [
        (130, FrameResourceHandle(130)),
        (131, FrameResourceHandle(131)),
        (132, FrameResourceHandle(132)),
    ] {
        let key = base_key(bucket);
        let transaction = promote_branch_entry(&mut working_set, key, handle, released.clone());
        let demote = transaction.supersede_to_recent(&mut working_set, lookup_request(key, 1_200));
        assert!(matches!(
            demote,
            TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
        ));
    }

    assert_eq!(working_set.recent_superseded_len(), 2);
    assert_eq!(release_count(&released, FrameResourceHandle(130)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(131)), 0);
    assert_eq!(release_count(&released, FrameResourceHandle(132)), 0);
}

#[test]
fn full_recent_compartment_does_not_evict_primary_hover_entry() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let primary_key = base_key(140);
    let mut working_set = working_set_with_recent(1, 1);
    working_set.insert_prepared_frame(
        primary_key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(140), released.clone()),
            timing(1_250),
        ),
    );

    for (bucket, handle) in [
        (141, FrameResourceHandle(141)),
        (142, FrameResourceHandle(142)),
    ] {
        let key = base_key(bucket);
        let mut transaction_source = working_set_with_recent(0, 0);
        let transaction =
            promote_branch_entry(&mut transaction_source, key, handle, released.clone());
        let demote = transaction.supersede_to_recent(&mut working_set, lookup_request(key, 1_200));
        assert!(matches!(
            demote,
            TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
        ));
    }

    assert_eq!(working_set.len(), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(140)), 0);
    assert_eq!(release_count(&released, FrameResourceHandle(141)), 1);

    let lookup = working_set.lookup_prepared_frame(lookup_request(primary_key, 1_200));
    assert!(matches!(lookup, TimelineHoverPrepareLookupOutcome::Hit(_)));
}

#[test]
fn pointer_movement_does_not_clear_recent_superseded() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let recent_key = base_key(150);
    let mut working_set = working_set_with_recent(1, 1);
    let transaction = promote_branch_entry(
        &mut working_set,
        recent_key,
        FrameResourceHandle(150),
        released.clone(),
    );
    let demote =
        transaction.supersede_to_recent(&mut working_set, lookup_request(recent_key, 1_200));
    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));

    working_set.insert_prepared_frame(
        base_key(151),
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(151), released.clone()),
            timing(1_260),
        ),
    );

    assert_eq!(working_set.recent_superseded_len(), 1);
    assert!(released_handles(&released).is_empty());
    assert!(matches!(
        working_set.lookup_prepared_frame(lookup_request(recent_key, 1_200)),
        TimelineHoverPrepareLookupOutcome::Hit(_)
    ));
}

#[test]
fn generation_clear_releases_recent_with_typed_reason_only() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let recent_key = base_key(160);
    let primary_key = base_key(161);
    let mut working_set = working_set_with_recent(1, 1);
    let transaction = promote_branch_entry(
        &mut working_set,
        recent_key,
        FrameResourceHandle(160),
        released.clone(),
    );
    let demote =
        transaction.supersede_to_recent(&mut working_set, lookup_request(recent_key, 1_200));
    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));
    working_set.insert_prepared_frame(
        primary_key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(161), released.clone()),
            timing(1_260),
        ),
    );

    let cleared = working_set
        .clear_recent_superseded(TimelineHoverRecentSupersededClearReason::GenerationChanged);

    assert_eq!(cleared, 1);
    assert_eq!(working_set.len(), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(160)), 1);
    assert_eq!(release_count(&released, FrameResourceHandle(161)), 0);
}

#[test]
fn recent_lookup_validates_actual_timing() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(170);
    let mut working_set = working_set_with_recent(1, 1);
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::new(
            hardware_lease(FrameResourceHandle(170), released.clone()),
            timing(900),
        )
        .with_branch_token(FakeBranchToken { branch_id: 170 }),
    );
    let promotion = working_set.promote_prepared_frame(lookup_request(key, 800));
    let TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(promoted_frame) = promotion
    else {
        panic!("entry must promote when demote validation target is before actual PTS");
    };
    let demote = working_set.try_demote_promoted_frame_to_recent_superseded(
        promoted_frame,
        lookup_request(key, 800),
        CancelScrubReason::SupersededByNewTarget,
    );
    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 950));

    assert!(matches!(
        lookup,
        TimelineHoverPrepareLookupOutcome::TimingRejected(
            TimelineHoverPrepareTimingRejection::ActualFrameBeforeRequestedTarget { .. }
        )
    ));
}

#[test]
fn primary_hover_entry_wins_over_matching_recent_superseded() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(180);
    let mut working_set = working_set_with_recent(1, 1);
    let transaction = promote_branch_entry(
        &mut working_set,
        key,
        FrameResourceHandle(180),
        released.clone(),
    );
    let demote = transaction.supersede_to_recent(&mut working_set, lookup_request(key, 1_200));
    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));
    working_set.insert_prepared_frame(
        key,
        TimelineHoverPreparedFrameEntry::<FakeBranchToken>::new(
            hardware_lease(FrameResourceHandle(181), released.clone()),
            timing(1_300),
        ),
    );

    let lookup = working_set.lookup_prepared_frame(lookup_request(key, 1_200));

    let TimelineHoverPrepareLookupOutcome::Hit(frame) = lookup else {
        panic!("primary hover entry must win when both primary and recent validate");
    };
    assert_eq!(
        frame.resource_descriptor().resource_handle(),
        FrameResourceHandle(181)
    );
    assert_eq!(working_set.recent_superseded_len(), 1);
}

#[test]
fn demoted_recent_entry_releases_once_after_working_set_drop() {
    let released = Arc::new(Mutex::new(Vec::new()));
    let key = base_key(190);
    let mut working_set = working_set_with_recent(1, 1);
    let transaction = promote_branch_entry(
        &mut working_set,
        key,
        FrameResourceHandle(190),
        released.clone(),
    );
    let demote = transaction.supersede_to_recent(&mut working_set, lookup_request(key, 1_200));
    assert!(matches!(
        demote,
        TimelineHoverPrepareDemoteBackOutcome::DemotedToRecentSuperseded
    ));
    assert!(released_handles(&released).is_empty());

    drop(working_set);

    assert_eq!(release_count(&released, FrameResourceHandle(190)), 1);
}
