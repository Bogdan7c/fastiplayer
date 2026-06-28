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
