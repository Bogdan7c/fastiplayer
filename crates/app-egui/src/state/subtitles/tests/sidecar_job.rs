//! Тесты фоновой задачи загрузки субтитров: выбор файла, лимиты, ошибки, отмена.

use std::io::Write;

use subtitle_core::SubtitleLanguage;
use subtitle_formats::{LegacyEncodingHint, SubtitleLoadError};

use super::super::job_runner::{SidecarJob, SidecarJobPoll};
use super::super::limits::SUBTITLE_FILE_MAX_BYTES;
use super::super::sidecar_job::{
    FontLibraryPlan, SidecarJobOutcome, SidecarJobRequest, SubtitleLoadFailure, encoding_hint_for,
    read_file_with_limit, run_sidecar_job,
};
use super::super::{JobResultDisposition, SubtitleLifecycleView};
use super::*;

fn request_for(video: &Path, fonts: FontLibraryPlan) -> SidecarJobRequest {
    SidecarJobRequest {
        video_path: video.to_path_buf(),
        fonts,
    }
}

#[test]
fn srt_next_to_the_video_is_loaded_with_its_cues_and_a_font_library() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));

    let outcome = run_sidecar_job(
        &request_for(&video, FontLibraryPlan::BuildEmbeddedOnly),
        &|| false,
    );

    let SidecarJobOutcome::Loaded(loaded) = outcome else {
        panic!("srt must be loaded");
    };
    assert_eq!(loaded.timeline.len(), 2, "both cues are in the timeline");
    assert!(loaded.fonts.is_some(), "a new library was requested");
    assert_eq!(
        loaded.file_name.as_deref().and_then(|name| name.to_str()),
        Some("Movie.srt")
    );
}

#[test]
fn reusing_the_existing_rasterizer_builds_no_font_library() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));

    let outcome = run_sidecar_job(
        &request_for(&video, FontLibraryPlan::ReuseExisting),
        &|| false,
    );

    let SidecarJobOutcome::Loaded(loaded) = outcome else {
        panic!("srt must be loaded");
    };
    assert!(
        loaded.fonts.is_none(),
        "no second font scan for the next media"
    );
}

#[test]
fn no_candidates_means_no_subtitles_without_any_error() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    std::fs::write(directory.path().join("Other.srt"), TWO_CUE_SRT).expect("write other");

    let outcome = run_sidecar_job(
        &request_for(&video, FontLibraryPlan::BuildEmbeddedOnly),
        &|| false,
    );
    assert!(matches!(
        outcome,
        SidecarJobOutcome::NoRenderableCandidate {
            skipped_unsupported_format: 0
        }
    ));

    // Тот же сценарий через владельца: состояние «нет файла», не «недоступно».
    let mut owner = owner();
    begin_and_wait(&mut owner, instance(1), &video);
    assert_eq!(
        owner.lifecycle_view(),
        SubtitleLifecycleView::NoSubtitleFile
    );
    assert!(owner.unavailable_failure().is_none());
}

#[test]
fn only_ass_is_skipped_and_counted() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    std::fs::write(directory.path().join("Movie.ass"), "[Script Info]\n").expect("write ass");

    let outcome = run_sidecar_job(
        &request_for(&video, FontLibraryPlan::BuildEmbeddedOnly),
        &|| false,
    );

    assert!(matches!(
        outcome,
        SidecarJobOutcome::NoRenderableCandidate {
            skipped_unsupported_format: 1
        }
    ));
}

#[test]
fn a_renderable_candidate_is_picked_even_when_ass_is_present() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    std::fs::write(directory.path().join("Movie.ru.ass"), "[Script Info]\n").expect("write ass");

    let outcome = run_sidecar_job(
        &request_for(&video, FontLibraryPlan::BuildEmbeddedOnly),
        &|| false,
    );

    let SidecarJobOutcome::Loaded(loaded) = outcome else {
        panic!("srt must be picked");
    };
    assert_eq!(loaded.timeline.len(), 2);
}

#[test]
fn oversized_file_is_a_typed_failure_that_leaves_playback_state_alone() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    let mut huge = std::fs::File::create(directory.path().join("Movie.srt")).expect("create");
    huge.write_all(&vec![b'a'; SUBTITLE_FILE_MAX_BYTES + 1])
        .expect("write huge");
    drop(huge);

    let mut owner = owner();
    begin_and_wait(&mut owner, instance(1), &video);

    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Unavailable);
    assert!(matches!(
        owner.unavailable_failure(),
        Some(SubtitleLoadFailure::FileTooLarge { limit_bytes }) if *limit_bytes == SUBTITLE_FILE_MAX_BYTES
    ));
    // Недоступная дорожка ничего не рисует.
    assert!(
        owner
            .overlay_for_frame(
                current(instance(1), 1),
                &frame(2_000, 1, full_rect(640, 360))
            )
            .is_none()
    );
}

#[test]
fn file_exactly_at_the_limit_is_read_in_full() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("Movie.srt");
    std::fs::write(&path, vec![b'a'; 1024]).expect("write");
    let bytes = read_file_with_limit(&path, 1024).expect("exactly at limit is allowed");
    assert_eq!(bytes.len(), 1024);
    assert!(matches!(
        read_file_with_limit(&path, 1023),
        Err(SubtitleLoadFailure::FileTooLarge { limit_bytes: 1023 })
    ));
}

#[test]
fn invalid_webvtt_is_a_parse_failure() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    std::fs::write(directory.path().join("Movie.vtt"), "this is not webvtt").expect("write vtt");

    let outcome = run_sidecar_job(
        &request_for(&video, FontLibraryPlan::BuildEmbeddedOnly),
        &|| false,
    );

    assert!(matches!(
        outcome,
        SidecarJobOutcome::Failed(SubtitleLoadFailure::ParseFailed(
            SubtitleLoadError::NotWebVtt
        ))
    ));
}

#[test]
fn missing_file_is_a_read_failure_with_the_io_kind() {
    let directory = tempfile::tempdir().expect("tempdir");
    let missing = directory.path().join("gone.srt");
    assert!(matches!(
        read_file_with_limit(&missing, 1024),
        Err(SubtitleLoadFailure::ReadFailed {
            kind: std::io::ErrorKind::NotFound
        })
    ));
}

#[cfg(unix)]
#[test]
fn unreadable_file_is_a_read_failure_and_the_owner_marks_the_track_unavailable() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    let srt = directory.path().join("Movie.srt");
    std::fs::set_permissions(&srt, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::File::open(&srt).is_ok() {
        // Права не действуют (например, запуск от root): сценарий недостижим.
        eprintln!("skip: permissions are not enforced for this user");
        return;
    }

    let mut owner = owner();
    begin_and_wait(&mut owner, instance(1), &video);

    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Unavailable);
    assert!(matches!(
        owner.unavailable_failure(),
        Some(SubtitleLoadFailure::ReadFailed {
            kind: std::io::ErrorKind::PermissionDenied
        })
    ));
}

#[test]
fn cancelled_job_publishes_cancellation_and_nothing_else() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));

    let outcome = run_sidecar_job(
        &request_for(&video, FontLibraryPlan::BuildEmbeddedOnly),
        &|| true,
    );

    assert!(matches!(outcome, SidecarJobOutcome::Cancelled));
}

#[test]
fn superseded_job_is_retired_and_its_result_never_reaches_the_new_media() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    let mut owner = owner();
    owner.begin_media(instance(1), SubtitleMediaSource::LocalFile(video));

    owner.clear_media();
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Idle);
    // Уходящая задача дожимается без блокировки и не меняет состояние владельца.
    let deadline = Instant::now() + LOAD_TIMEOUT;
    while owner.retiring_job_count() > 0 {
        assert!(Instant::now() < deadline, "retiring job must finish");
        assert!(
            !owner.poll_jobs(),
            "a retired job never changes visible state"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Idle);
}

#[test]
fn job_delivers_its_result_through_the_owner_mailbox() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    let job = SidecarJob::spawn(
        request_for(&video, FontLibraryPlan::BuildEmbeddedOnly),
        AppWakePort::disconnected(AppWakeOwner::Subtitles),
    )
    .expect("spawn");

    let deadline = Instant::now() + LOAD_TIMEOUT;
    let outcome = loop {
        match job.poll() {
            SidecarJobPoll::Finished(outcome) => break outcome,
            SidecarJobPoll::Pending => {
                assert!(Instant::now() < deadline, "job must finish");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    };
    assert!(matches!(outcome, SidecarJobOutcome::Loaded(_)));
}

#[test]
fn encoding_hint_follows_the_declared_language() {
    let russian = SubtitleLanguage::parse("ru").expect("ru");
    let english = SubtitleLanguage::parse("en").expect("en");
    assert_eq!(encoding_hint_for(None), LegacyEncodingHint::Cyrillic);
    assert_eq!(
        encoding_hint_for(Some(&russian)),
        LegacyEncodingHint::Cyrillic
    );
    assert_eq!(
        encoding_hint_for(Some(&english)),
        LegacyEncodingHint::Generic
    );
}

#[test]
fn late_failed_result_for_an_old_media_is_dropped_too() {
    let mut owner = owner();
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    owner.begin_media(instance(2), SubtitleMediaSource::LocalFile(video));

    let disposition = owner.accept_job_outcome(
        instance(1),
        SidecarJobOutcome::Failed(SubtitleLoadFailure::WorkerLost),
    );

    assert_eq!(disposition, JobResultDisposition::DroppedStale);
    assert!(owner.unavailable_failure().is_none());
}
