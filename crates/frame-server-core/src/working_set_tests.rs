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
