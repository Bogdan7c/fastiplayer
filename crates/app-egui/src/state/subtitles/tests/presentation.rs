//! Функциональные тесты решения «что рисовать на кадре»: пауза, seek, смена media,
//! letterbox, отсутствие работы на кадрах без реплик.

use std::sync::Arc;

use subtitle_core::SubtitleSyncOffset;
use subtitle_raster::{CueSourceId, SubtitleAppearance, SubtitleOverlay};

use super::super::user_style::UserSubtitleStyle;

use super::super::active_track::ActiveSubtitleTrack;
use super::super::presentation::SubtitlePresenter;
use super::super::sidecar_job::{FontLibraryPlan, SidecarJobOutcome};
use super::super::{JobOrigin, JobResultDisposition, SubtitleLifecycleView, SubtitleMediaSource};
use super::*;

const VIEW_WIDTH: u32 = 1280;
const VIEW_HEIGHT: u32 = 720;
/// Миллисекунды: середина первой и второй реплики и пауза между ними.
const INSIDE_CUE_ONE: u64 = 2_000;
const BETWEEN_CUES: u64 = 4_000;
const INSIDE_CUE_TWO: u64 = 6_000;
const GENERATION: u64 = 3;

/// Отпечаток overlay для сравнения «тот же / другой»: ревизия и адрес первой маски.
fn fingerprint(overlay: &SubtitleOverlay) -> (u64, usize) {
    let first_mask = overlay.bitmaps().first().map_or(0, |bitmap| {
        Arc::as_ptr(bitmap.shared_coverage()) as *const u8 as usize
    });
    (overlay.revision().value(), first_mask)
}

#[test]
fn cue_is_shown_only_while_the_frame_pts_is_inside_it() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    let rect = full_rect(VIEW_WIDTH, VIEW_HEIGHT);

    let first = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_ONE, GENERATION, rect),
        )
        .map(fingerprint)
        .expect("first cue must produce an overlay");
    assert!(
        owner
            .overlay_for_frame(
                current(media, GENERATION),
                &frame(BETWEEN_CUES, GENERATION, rect)
            )
            .is_none(),
        "no overlay between cues"
    );
    let second = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_TWO, GENERATION, rect),
        )
        .map(fingerprint)
        .expect("second cue must produce an overlay");

    assert_ne!(first, second, "different cues must give different overlays");
}

#[test]
fn pause_on_the_same_frame_rasterizes_once_and_reuses_the_same_overlay() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    let rect = full_rect(VIEW_WIDTH, VIEW_HEIGHT);
    let paused_frame = frame(INSIDE_CUE_ONE, GENERATION, rect);

    let first = owner
        .overlay_for_frame(current(media, GENERATION), &paused_frame)
        .map(fingerprint)
        .expect("overlay on the paused frame");
    for _ in 0..30 {
        let again = owner
            .overlay_for_frame(current(media, GENERATION), &paused_frame)
            .map(fingerprint)
            .expect("overlay stays while paused");
        assert_eq!(again, first, "paused frame must keep the very same overlay");
    }
    // Воспроизведение внутри той же реплики (pts растёт) тоже не растеризует заново.
    for pts in [2_100, 2_200, 2_900] {
        let moving = owner
            .overlay_for_frame(current(media, GENERATION), &frame(pts, GENERATION, rect))
            .map(fingerprint)
            .expect("overlay inside the same cue");
        assert_eq!(moving, first);
    }

    let stats = owner.presentation_stats();
    assert_eq!(stats.rasterizer_calls, 1, "rasterized exactly once");
    assert_eq!(
        stats.reused_frames, 33,
        "every other frame reused the overlay"
    );
    assert_eq!(
        owner.rasterizer_statistics().map(|s| s.rendered_frames),
        Some(1),
        "the rasterizer itself built a single frame"
    );
}

#[test]
fn seek_past_the_cue_hides_it_on_the_very_first_frame_and_seeking_back_restores_it() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    let rect = full_rect(VIEW_WIDTH, VIEW_HEIGHT);

    let before = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_ONE, GENERATION, rect),
        )
        .map(fingerprint)
        .expect("cue one");
    // Seek за конец реплики: самый первый кадр после seek уже без реплики.
    assert!(
        owner
            .overlay_for_frame(
                current(media, GENERATION),
                &frame(BETWEEN_CUES, GENERATION, rect)
            )
            .is_none()
    );
    // Seek во вторую реплику: показывается именно она.
    let second = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_TWO, GENERATION, rect),
        )
        .map(fingerprint)
        .expect("cue two");
    assert_ne!(second, before);
    // Seek назад в первую: реплика возвращается новым overlay (после `None` между ними).
    let back = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_ONE, GENERATION, rect),
        )
        .map(fingerprint)
        .expect("cue one again");
    assert_ne!(back, second);
}

#[test]
fn frames_of_another_media_or_generation_never_get_subtitles() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    let rect = full_rect(VIEW_WIDTH, VIEW_HEIGHT);
    let cue_frame = frame(INSIDE_CUE_ONE, GENERATION, rect);

    // Контроль: при совпадении media и поколения реплика есть, значит `None` ниже — не сбой фикстуры.
    assert!(
        owner
            .overlay_for_frame(current(media, GENERATION), &cue_frame)
            .is_some()
    );
    // Snapshot уже показывает другое media: реплики прежнего не рисуются ни кадра.
    assert!(
        owner
            .overlay_for_frame(current(instance(2), GENERATION), &cue_frame)
            .is_none()
    );
    // Snapshot без media (после Stop/reset).
    let no_media = CurrentMediaForSubtitles {
        media_instance_id: None,
        render_generation: GENERATION,
    };
    assert!(owner.overlay_for_frame(no_media, &cue_frame).is_none());
    // Кадр прошлого поколения ресурсов рендера (например, кэш до смены media).
    assert!(
        owner
            .overlay_for_frame(current(media, GENERATION + 1), &cue_frame)
            .is_none()
    );
}

#[test]
fn new_media_drops_the_previous_track_at_once_and_late_results_of_the_old_media_are_ignored() {
    let media_a = instance(1);
    let media_b = instance(2);
    let (mut owner, directory) = ready_owner(media_a);
    let rect = full_rect(VIEW_WIDTH, VIEW_HEIGHT);

    // B установлено: дорожка A исчезает мгновенно, даже если B ещё грузится.
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    owner.begin_media(media_b, SubtitleMediaSource::LocalFile(video));
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Loading);
    for media in [media_a, media_b] {
        assert!(
            owner
                .overlay_for_frame(
                    current(media, GENERATION),
                    &frame(INSIDE_CUE_ONE, GENERATION, rect)
                )
                .is_none(),
            "no subtitles while the new media is loading"
        );
    }

    // Поздний результат задачи A (готовый Loaded) не должен применяться к B.
    let late = loaded_outcome();
    assert_eq!(
        owner.accept_job_outcome(media_a, JobOrigin::Discovery, late),
        JobResultDisposition::DroppedStale
    );
    assert_eq!(
        owner.lifecycle_view(),
        SubtitleLifecycleView::Loading,
        "late result of A must not touch the state of B"
    );
}

/// Готовый результат загрузки (как у фоновой задачи) с двумя репликами.
fn loaded_outcome() -> SidecarJobOutcome {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    let request = discovery_request(&video, FontLibraryPlan::BuildEmbeddedOnly);
    let outcome = super::super::sidecar_job::run_sidecar_job(&request, &|| false);
    assert!(matches!(&outcome, SidecarJobOutcome::Discovered(d) if d.preloaded.is_some()));
    outcome
}

#[test]
fn letterboxed_picture_gets_a_canvas_of_the_picture_and_every_bitmap_stays_inside_it() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    let rect = letterboxed_rect(VIEW_WIDTH, VIEW_HEIGHT);

    let overlay = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_ONE, GENERATION, rect),
        )
        .expect("overlay on the letterboxed picture");

    // Холст — округлённая картинка видео, а не всё окно (полосы в холст не входят).
    let canvas = overlay.canvas();
    assert_eq!(canvas.width(), rect.width.round() as u32);
    assert_eq!(canvas.height(), rect.height.round() as u32);
    assert!(canvas.height() < VIEW_HEIGHT, "canvas excludes the bars");
    // Каждая картинка целиком внутри холста, значит и внутри картинки видео.
    assert!(!overlay.bitmaps().is_empty());
    for bitmap in overlay.bitmaps() {
        let origin = bitmap.origin();
        assert!(
            origin.x >= 0 && origin.y >= 0,
            "bitmap starts inside the canvas"
        );
        assert!(origin.x as u32 + bitmap.width() <= canvas.width());
        assert!(origin.y as u32 + bitmap.height() <= canvas.height());
    }
}

#[test]
fn changing_the_picture_size_or_the_style_rasterizes_again() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    let small = full_rect(640, 360);
    let large = full_rect(VIEW_WIDTH, VIEW_HEIGHT);

    let small_overlay = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_ONE, GENERATION, small),
        )
        .map(|overlay| (fingerprint(overlay), overlay.canvas().width()))
        .expect("small");
    let large_overlay = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_ONE, GENERATION, large),
        )
        .map(|overlay| (fingerprint(overlay), overlay.canvas().width()))
        .expect("large");
    assert_eq!(small_overlay.1, 640);
    assert_eq!(large_overlay.1, VIEW_WIDTH);
    assert_ne!(small_overlay.0, large_overlay.0);
    assert_eq!(owner.presentation_stats().rasterizer_calls, 2);

    owner.replace_appearance(SubtitleAppearance::outline_and_shadow());
    let restyled = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_ONE, GENERATION, large),
        )
        .map(fingerprint)
        .expect("restyled");
    assert_ne!(restyled, large_overlay.0);
    assert_eq!(owner.presentation_stats().rasterizer_calls, 3);
}

#[test]
fn frames_without_active_cues_do_no_rasterizer_work() {
    let media = instance(1);
    let (mut owner, _dir) = ready_owner(media);
    let rect = full_rect(VIEW_WIDTH, VIEW_HEIGHT);

    for pts in [0, 500, BETWEEN_CUES, 4_500, 9_000, 60_000] {
        assert!(
            owner
                .overlay_for_frame(current(media, GENERATION), &frame(pts, GENERATION, rect))
                .is_none(),
            "no cue at {pts} ms"
        );
    }
    assert_eq!(owner.presentation_stats().rasterizer_calls, 0);
    assert_eq!(
        owner.rasterizer_statistics().map(|s| s.rendered_frames),
        Some(0),
        "rasterizer never asked to draw"
    );
}

#[test]
fn owner_without_any_track_never_produces_an_overlay() {
    let mut owner = owner();
    let rect = full_rect(VIEW_WIDTH, VIEW_HEIGHT);
    let media = instance(1);
    assert!(
        owner
            .overlay_for_frame(
                current(media, GENERATION),
                &frame(INSIDE_CUE_ONE, GENERATION, rect)
            )
            .is_none()
    );
    // Не локальный источник: штатное «без субтитров», задача не запускалась.
    owner.begin_media(media, SubtitleMediaSource::NotLocal);
    assert_eq!(
        owner.lifecycle_view(),
        SubtitleLifecycleView::NoSubtitleFile
    );
    assert!(
        owner
            .overlay_for_frame(
                current(media, GENERATION),
                &frame(INSIDE_CUE_ONE, GENERATION, rect)
            )
            .is_none()
    );
    assert_eq!(owner.retiring_job_count(), 0, "no job for non-local media");
    assert_eq!(owner.presentation_stats().rasterizer_calls, 0);
}

#[test]
fn sync_offset_moves_the_lookup_time_of_a_frame() {
    // Презентатор напрямую: дорожка со сдвигом +2 с (субтитры позже на 2 с).
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    let request = discovery_request(&video, FontLibraryPlan::BuildEmbeddedOnly);
    let discovery = expect_discovery(super::super::sidecar_job::run_sidecar_job(
        &request,
        &|| false,
    ));
    let timeline = discovery
        .preloaded
        .expect("track is read")
        .result
        .expect("track must load")
        .timeline;
    let config = SubtitlesConfig::default();
    let mut presenter = SubtitlePresenter::new(
        None,
        &UserSubtitleStyle::from_config(&config),
        super::super::limits::raster_limits_from_config(&config),
    );
    presenter.install_font_library(discovery.fonts.expect("fonts requested"));
    let delayed = ActiveSubtitleTrack::new(
        CueSourceId::new(7),
        timeline,
        SubtitleSyncOffset::from_millis(2_000),
        None,
    );
    let rect = full_rect(VIEW_WIDTH, VIEW_HEIGHT);

    // Кадр 4.0 с при сдвиге +2 с ищет реплику на 2.0 с: первая реплика видна.
    assert!(
        presenter
            .overlay_for_frame(&delayed, &frame(4_000, GENERATION, rect))
            .is_some()
    );
    // Кадр 2.0 с ищет на 0.0 с: реплик нет.
    assert!(
        presenter
            .overlay_for_frame(&delayed, &frame(2_000, GENERATION, rect))
            .is_none()
    );
}
