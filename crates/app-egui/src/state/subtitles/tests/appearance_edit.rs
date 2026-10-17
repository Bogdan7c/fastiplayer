//! Режим правки внешнего вида: от намерений владельца до пикселей.
//!
//! Проверяется то, что видит зритель и что попадёт в конфиг: живое применение черновика,
//! «одна перерастеризация на смену размера и ни одной на смену позиции», точный откат по Esc,
//! образец без дорожки, приоритет настоящей реплики, неявный выход по закрытию видео.
//! GPU-тест пропускается (с сообщением) без adapter-а.

use fastiplayer_config::SubtitleDecorationPreference;
use render_core::{PictureRect, RenderViewport};

use super::gpu_chain::Harness;
use super::*;
use crate::state::subtitles::appearance_edit::{
    AppearanceEdit, AppearanceEditBegin, AppearanceEditChange, AppearanceEditFinish,
    AppearanceEditOutcome,
};
use crate::state::subtitles::controls_obstruction::BottomControlsObstruction;
use crate::state::subtitles::user_style::{SubtitlePosition, SubtitleRgb, UserSubtitleStyle};

const GENERATION: u64 = 5;
const WIDTH: u32 = 256;
const HEIGHT: u32 = 144;
/// Кадр внутри первой реплики `TWO_CUE_SRT` ([1 с, 3 с)).
pub(super) const INSIDE_CUE_MILLIS: u64 = 2_000;
/// Кадр между репликами: настоящей реплики нет.
const BETWEEN_CUES_MILLIS: u64 = 4_000;

fn frame_at(pts_millis: u64, generation: u64) -> SubmittedFrameForSubtitles {
    SubmittedFrameForSubtitles::for_test_with_viewport(
        Duration::from_millis(pts_millis),
        generation,
        full_rect(WIDTH, HEIGHT),
        RenderViewport::full_surface(WIDTH, HEIGHT),
    )
}

/// Сдвиг слоя и ширина содержимого на кадре; `None`, если слоя нет.
pub(super) fn placed_at(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    pts: u64,
) -> Option<(f32, f32, u32)> {
    let placed =
        owner.placed_overlay_for_frame(current(media, GENERATION), &frame_at(pts, GENERATION))?;
    let width = placed.overlay.content_bounds()?.width;
    Some((
        placed.layer_offsets.main.x,
        placed.layer_offsets.main.y,
        width,
    ))
}

/// Пиксели overlay на кадре (RGBA на прозрачном фоне) вместе с размещением.
fn pixels_at(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    pts: u64,
) -> Option<(Vec<u8>, f32, f32)> {
    let placed =
        owner.placed_overlay_for_frame(current(media, GENERATION), &frame_at(pts, GENERATION))?;
    Some((
        placed.overlay.composite_rgba_over_transparent(),
        placed.layer_offsets.main.x,
        placed.layer_offsets.main.y,
    ))
}

pub(super) fn editing_owner() -> (SubtitleOwner, MediaInstanceId, tempfile::TempDir) {
    let media = instance(1);
    let (mut owner, directory) = ready_owner(media);
    assert_eq!(owner.begin_appearance_edit(), AppearanceEditBegin::Started);
    (owner, media, directory)
}

/// Владелец с видео без дорожек: идёт только поиск, который ничего не находит.
fn owner_with_media_without_tracks(media: MediaInstanceId) -> (SubtitleOwner, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    let mut owner = owner();
    begin_and_wait(&mut owner, media, &video);
    assert_eq!(
        owner.lifecycle_view(),
        SubtitleLifecycleView::NoSubtitleFile
    );
    (owner, directory)
}

/// Ждёт, пока фоновое построение шрифтов даст растеризатор.
fn wait_for_rasterizer(owner: &mut SubtitleOwner) {
    let deadline = Instant::now() + LOAD_TIMEOUT;
    while !owner.presenter.has_rasterizer() {
        assert!(Instant::now() < deadline, "fonts were not prepared in time");
        owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn begin_requires_media_and_reports_an_already_open_session() {
    let mut idle = owner();
    assert_eq!(idle.begin_appearance_edit(), AppearanceEditBegin::NoMedia);
    assert!(!idle.is_editing_appearance());

    let (mut owner, _media, _directory) = editing_owner();
    assert_eq!(
        owner.begin_appearance_edit(),
        AppearanceEditBegin::AlreadyActive
    );
    assert!(owner.is_editing_appearance());
}

#[test]
fn every_edit_intent_without_a_session_changes_nothing() {
    let media = instance(1);
    let (mut owner, _directory) = ready_owner(media);
    let calls_before = {
        placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
        owner.presentation_stats().rasterizer_calls
    };

    assert_eq!(
        owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 200 }),
        AppearanceEditChange::NotEditing
    );
    assert_eq!(
        owner.reset_appearance_draft(),
        AppearanceEditChange::NotEditing
    );
    assert_eq!(
        owner.finish_appearance_edit(AppearanceEditFinish::Save),
        AppearanceEditOutcome::NotEditing
    );
    assert_eq!(
        owner.finish_appearance_edit(AppearanceEditFinish::Cancel),
        AppearanceEditOutcome::NotEditing
    );

    assert!(owner.appearance_edit_view().is_none());
    assert!(
        owner.take_style_save_request().is_none(),
        "no-session Save queues nothing"
    );
    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
    assert_eq!(owner.presentation_stats().rasterizer_calls, calls_before);
}

#[test]
fn moving_changes_the_stored_position_and_only_the_offset() {
    let (mut owner, media, _directory) = editing_owner();
    let (_, y_before, _) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
    let calls_before = owner.presentation_stats().rasterizer_calls;

    let change = owner.apply_appearance_edit(AppearanceEdit::MoveBy {
        horizontal_fraction: -0.10,
        vertical_fraction: 0.20,
    });

    assert_eq!(change, AppearanceEditChange::Changed);
    let draft = owner.appearance_edit_view().expect("session").draft;
    assert_eq!(draft.position.horizontal_percent, 40, "50 % - 10 %");
    assert_eq!(draft.position.vertical_percent, 26, "6 % + 20 % (up)");
    let (x_after, y_after, _) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
    let picture_height = HEIGHT as f32;
    assert!(
        ((y_before - y_after) - 0.20 * picture_height).abs() <= 1.0,
        "layer moved up by 0.2 of the picture: {y_before} -> {y_after}"
    );
    assert!(x_after < 0.0, "layer moved left");
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_before,
        "position is a GPU offset: no re-rasterization"
    );
}

#[test]
fn grabbing_a_lifted_layer_starts_the_move_from_where_it_is_shown() {
    let (mut owner, media, _directory) = editing_owner();
    // Панель управления закрывает низ кадра: слой поднят над ней.
    owner.observe_bottom_controls_obstruction(BottomControlsObstruction::TopEdge {
        top_px: 100,
        clearance_px: 0,
    });
    let (_, lifted_y, _) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
    assert!(lifted_y < 0.0, "the panel lifts the layer: {lifted_y}");
    let calls_before = owner.presentation_stats().rasterizer_calls;

    assert_eq!(
        owner.start_appearance_layer_move(),
        AppearanceEditChange::Changed,
        "the stored position jumps to the shown one"
    );
    owner.apply_appearance_edit(AppearanceEdit::MoveBy {
        horizontal_fraction: 0.0,
        vertical_fraction: 0.10,
    });

    let (_, moved_y, _) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
    // Без захвата перенос считался бы от сохранённых 6 % и «съелся» бы подъёмом: слой стоял бы.
    assert!(
        ((lifted_y - moved_y) - 0.10 * HEIGHT as f32).abs() <= 1.0,
        "the first pixels of the drag move the layer: {lifted_y} -> {moved_y}"
    );
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_before,
        "grabbing and moving change only the GPU offset"
    );
}

#[test]
fn grabbing_without_a_session_or_before_the_layer_was_shown_changes_nothing() {
    let media = instance(1);
    let (mut owner, _directory) = ready_owner(media);
    assert_eq!(
        owner.start_appearance_layer_move(),
        AppearanceEditChange::NotEditing
    );

    assert_eq!(owner.begin_appearance_edit(), AppearanceEditBegin::Started);
    let before = owner.appearance_edit_view().expect("session").draft;
    // Ни одного кадра ещё не размещено: брать «видимое место» неоткуда.
    assert_eq!(
        owner.start_appearance_layer_move(),
        AppearanceEditChange::Unchanged
    );
    assert_eq!(owner.appearance_edit_view().expect("session").draft, before);
}

#[test]
fn grabbing_an_unobstructed_layer_keeps_the_stored_position() {
    let (mut owner, media, _directory) = editing_owner();
    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
    assert_eq!(
        owner.start_appearance_layer_move(),
        AppearanceEditChange::Unchanged,
        "shown place == stored place: nothing to rebase"
    );
    let draft = owner.appearance_edit_view().expect("session").draft;
    assert_eq!(draft.position, UserSubtitleStyle::default().position);
}

#[test]
fn small_drags_accumulate_instead_of_rounding_away() {
    let (mut owner, _media, _directory) = editing_owner();
    // Двадцать шагов по 0.4 % ширины: по отдельности округлились бы в ноль.
    for _ in 0..20 {
        owner.apply_appearance_edit(AppearanceEdit::MoveBy {
            horizontal_fraction: 0.004,
            vertical_fraction: 0.0,
        });
    }
    let draft = owner.appearance_edit_view().expect("session").draft;
    assert_eq!(draft.position.horizontal_percent, 58, "50 % + 20 * 0.4 %");
}

#[test]
fn size_and_colour_changes_rasterize_exactly_once_each() {
    let (mut owner, media, _directory) = editing_owner();
    let (_, _, width_before) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    let calls_before = owner.presentation_stats().rasterizer_calls;

    assert_eq!(
        owner.apply_appearance_edit(AppearanceEdit::ScaleTextBy { factor: 1.5 }),
        AppearanceEditChange::Changed
    );
    let (_, _, width_after) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("same frame again");
    assert!(width_after > width_before, "bigger text is wider");
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_before + 1
    );

    // Тот же размер ещё раз: изменений нет, перерастеризации нет.
    assert_eq!(
        owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 150 }),
        AppearanceEditChange::Unchanged
    );
    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_before + 1
    );

    // Цвет плашки - тоже одна перерастеризация (проверено ниже через значение черновика).
    let red = SubtitleRgb {
        red: 255,
        green: 0,
        blue: 0,
    };
    assert_eq!(
        owner.apply_appearance_edit(AppearanceEdit::SetTextColor(red)),
        AppearanceEditChange::Changed
    );
    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_before + 2
    );

    let blue = SubtitleRgb {
        red: 0,
        green: 0,
        blue: 255,
    };
    assert_eq!(
        owner.apply_appearance_edit(AppearanceEdit::SetBackgroundColor(blue)),
        AppearanceEditChange::Changed
    );
    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_before + 3
    );
    assert_eq!(
        owner
            .appearance_edit_view()
            .expect("session")
            .draft
            .background_color,
        blue
    );
}

#[test]
fn values_are_clamped_to_the_config_ranges() {
    let (mut owner, _media, _directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 1_000 });
    owner.apply_appearance_edit(AppearanceEdit::SetBackgroundOpacity { percent: 200 });
    owner.apply_appearance_edit(AppearanceEdit::MoveBy {
        horizontal_fraction: 9.0,
        vertical_fraction: -9.0,
    });
    let draft = owner.appearance_edit_view().expect("session").draft;
    assert_eq!(draft.text_size_percent, 250);
    assert_eq!(draft.background_opacity_percent, 100);
    assert_eq!(draft.position.horizontal_percent, 100);
    assert_eq!(draft.position.vertical_percent, -40);

    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 1 });
    assert_eq!(
        owner
            .appearance_edit_view()
            .expect("session")
            .draft
            .text_size_percent,
        50
    );
    // Нечисловые доли и множитель игнорируются.
    assert_eq!(
        owner.apply_appearance_edit(AppearanceEdit::ScaleTextBy { factor: f32::NAN }),
        AppearanceEditChange::Unchanged
    );
    assert_eq!(
        owner.apply_appearance_edit(AppearanceEdit::MoveBy {
            horizontal_fraction: f32::INFINITY,
            vertical_fraction: f32::NAN,
        }),
        AppearanceEditChange::Unchanged
    );
}

#[test]
fn cancel_restores_the_exact_style_and_pixels_from_before_entering() {
    let media = instance(1);
    let (mut owner, _directory) = ready_owner(media);
    let (pixels_before, x_before, y_before) =
        pixels_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
    let committed_style = UserSubtitleStyle::default();

    assert_eq!(owner.begin_appearance_edit(), AppearanceEditBegin::Started);
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 220 });
    owner.apply_appearance_edit(AppearanceEdit::SetDecoration(
        SubtitleDecorationPreference::Outline,
    ));
    owner.apply_appearance_edit(AppearanceEdit::MoveBy {
        horizontal_fraction: 0.2,
        vertical_fraction: 0.3,
    });
    let (pixels_edited, _, _) = pixels_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_ne!(pixels_edited, pixels_before, "the edit is visible live");

    assert_eq!(
        owner.finish_appearance_edit(AppearanceEditFinish::Cancel),
        AppearanceEditOutcome::Cancelled
    );

    let (pixels_after, x_after, y_after) =
        pixels_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(
        pixels_after, pixels_before,
        "overlay pixels are exactly the old ones"
    );
    assert_eq!(
        (x_after, y_after),
        (x_before, y_before),
        "placement is exactly the old one"
    );
    assert!(
        owner.take_style_save_request().is_none(),
        "Cancel never queues a save"
    );
    assert_eq!(owner.settings.style, committed_style);
    assert!(!owner.is_editing_appearance());
}

#[test]
fn reset_restores_defaults_keeps_the_ass_flag_and_the_session() {
    let (mut owner, media, _directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 200 });
    owner.apply_appearance_edit(AppearanceEdit::SetPosition(SubtitlePosition {
        horizontal_percent: 10,
        vertical_percent: 70,
    }));
    owner.settings.style.ass_force_user_style = true;
    // Флаг ASS живёт в черновике с момента входа: пересоздаём сессию с ним.
    owner.finish_appearance_edit(AppearanceEditFinish::Cancel);
    owner.settings.style.ass_force_user_style = true;
    assert_eq!(owner.begin_appearance_edit(), AppearanceEditBegin::Started);
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 200 });

    assert_eq!(
        owner.reset_appearance_draft(),
        AppearanceEditChange::Changed
    );

    let draft = owner.appearance_edit_view().expect("mode stays open").draft;
    let expected = UserSubtitleStyle {
        ass_force_user_style: true,
        ..UserSubtitleStyle::default()
    };
    assert_eq!(draft, expected);
    assert_eq!(
        owner.reset_appearance_draft(),
        AppearanceEditChange::Unchanged
    );
    let (_, y, _) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(y, 0.0, "default position means no offset");
}

#[test]
fn sample_is_shown_without_any_track_and_disappears_after_finishing() {
    let media = instance(1);
    let (mut owner, _directory) = owner_with_media_without_tracks(media);
    assert!(
        placed_at(&mut owner, media, INSIDE_CUE_MILLIS).is_none(),
        "nothing to show outside the edit"
    );

    assert_eq!(owner.begin_appearance_edit(), AppearanceEditBegin::Started);
    // Растеризатора ещё нет: он строится в фоне, образец появится чуть позже.
    wait_for_rasterizer(&mut owner);

    let (_, _, width) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("sample overlay");
    assert!(width > 0);
    let view = owner.appearance_edit_view().expect("session");
    let rect = view
        .layer_rect
        .expect("the frame around the sample has a rectangle");
    assert_eq!(rect.width, width);

    // Образец виден и между репликами, и на любом pts (реплики настоящей дорожки нет вовсе).
    placed_at(&mut owner, media, BETWEEN_CUES_MILLIS).expect("sample at any time");

    owner.finish_appearance_edit(AppearanceEditFinish::Cancel);
    assert!(placed_at(&mut owner, media, INSIDE_CUE_MILLIS).is_none());
    assert_eq!(owner.last_placed_layer_rect(), None);
}

#[test]
fn a_real_cue_wins_over_the_sample_and_the_sample_fills_the_gaps() {
    let media = instance(1);
    let (mut owner, _directory) = ready_owner(media);
    let (_, _, real_width) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("real cue");
    assert!(placed_at(&mut owner, media, BETWEEN_CUES_MILLIS).is_none());

    assert_eq!(owner.begin_appearance_edit(), AppearanceEditBegin::Started);
    let (_, _, width_in_cue) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("overlay");
    assert_eq!(
        width_in_cue, real_width,
        "the real cue is shown, not the longer sample"
    );

    let (_, _, width_in_gap) = placed_at(&mut owner, media, BETWEEN_CUES_MILLIS).expect("sample");
    assert!(
        width_in_gap > real_width,
        "the sample text is longer than 'First cue'"
    );
}

#[test]
fn the_edit_overlay_still_requires_matching_media_and_render_generation() {
    let (mut owner, media, _directory) = editing_owner();
    assert!(placed_at(&mut owner, media, BETWEEN_CUES_MILLIS).is_some());

    let other_media = owner.placed_overlay_for_frame(
        current(instance(2), GENERATION),
        &frame_at(BETWEEN_CUES_MILLIS, GENERATION),
    );
    assert!(other_media.is_none(), "another media never gets the sample");
    let stale_generation = owner.placed_overlay_for_frame(
        current(media, GENERATION),
        &frame_at(BETWEEN_CUES_MILLIS, GENERATION - 1),
    );
    assert!(
        stale_generation.is_none(),
        "a frame of another render generation is hidden"
    );
}

#[test]
fn save_queues_the_draft_once_and_keeps_showing_it_until_confirmation() {
    let (mut owner, media, _directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 180 });
    let (_, _, width_draft) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    let draft = owner.appearance_edit_view().expect("session").draft;

    assert_eq!(
        owner.finish_appearance_edit(AppearanceEditFinish::Save),
        AppearanceEditOutcome::SaveRequested
    );

    assert!(!owner.is_editing_appearance());
    let (_, _, width_after_save) = placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(
        width_after_save, width_draft,
        "no jump back before the config confirms"
    );
    assert_eq!(owner.take_style_save_request(), Some(draft));
    assert_eq!(owner.take_style_save_request(), None, "handed out once");
}

#[test]
fn confirmed_config_equal_to_the_saved_draft_does_not_rasterize_again() {
    let (mut owner, media, _directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 180 });
    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    let calls_after_edit = owner.presentation_stats().rasterizer_calls;
    owner.finish_appearance_edit(AppearanceEditFinish::Save);
    let saved = owner.take_style_save_request().expect("queued");

    // Подтверждение конфига после успешной записи.
    let mut committed = enabled_config();
    committed.text_size_percent = saved.text_size_percent;
    owner.apply_config(&committed);

    placed_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_after_edit
    );
    assert_eq!(owner.settings.style.text_size_percent, 180);
}

#[test]
fn failed_save_returns_to_the_committed_style() {
    let (mut owner, media, _directory) = editing_owner();
    let (pixels_committed, _, _) = pixels_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 200 });
    owner.finish_appearance_edit(AppearanceEditFinish::Save);
    owner.take_style_save_request().expect("queued");

    owner.discard_unsaved_style();

    let (pixels_after, _, _) = pixels_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(
        pixels_after, pixels_committed,
        "the committed style is on screen again"
    );
}

#[test]
fn an_unrelated_config_commit_does_not_overwrite_the_draft_and_cancel_goes_to_the_new_committed_style()
 {
    let (mut owner, media, _directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 200 });

    // Другая транзакция настроек во время правки: поменяла цвет и список языков.
    let mut committed = enabled_config();
    committed.text_color = "#FFFF00".to_owned();
    committed.preferred_languages = "uk".to_owned();
    owner.apply_config(&committed);

    let draft = owner
        .appearance_edit_view()
        .expect("session survives")
        .draft;
    assert_eq!(draft.text_size_percent, 200, "the draft is not overwritten");
    assert_eq!(
        draft.text_color,
        UserSubtitleStyle::default().text_color,
        "draft keeps its own colour"
    );
    assert_eq!(
        owner.settings.preferred_language_codes,
        vec!["uk"],
        "languages apply as before"
    );

    owner.finish_appearance_edit(AppearanceEditFinish::Cancel);
    let yellow = SubtitleRgb {
        red: 255,
        green: 255,
        blue: 0,
    };
    assert_eq!(owner.settings.style.text_color, yellow);
    let (pixels, _, _) = pixels_at(&mut owner, media, INSIDE_CUE_MILLIS).expect("cue");
    let mut reference = owner_with_config(&committed);
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    begin_and_wait(&mut reference, media, &video);
    let (reference_pixels, _, _) =
        pixels_at(&mut reference, media, INSIDE_CUE_MILLIS).expect("cue");
    assert_eq!(
        pixels, reference_pixels,
        "Cancel shows the CURRENT committed style"
    );
}

#[test]
fn clearing_the_media_ends_the_session_with_a_save() {
    let (mut owner, _media, _directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 130 });

    owner.clear_media();

    assert!(!owner.is_editing_appearance());
    let queued = owner
        .take_style_save_request()
        .expect("implicit exit saves");
    assert_eq!(queued.text_size_percent, 130);
}

#[test]
fn switching_to_another_video_keeps_the_session() {
    let (mut owner, _media, directory) = editing_owner();
    owner.apply_appearance_edit(AppearanceEdit::SetTextSize { percent: 130 });
    let next_video = write_movie(directory.path(), Some(TWO_CUE_SRT));

    owner.begin_media(instance(2), SubtitleMediaSource::LocalFile(next_video));

    assert!(owner.is_editing_appearance());
    assert_eq!(
        owner
            .appearance_edit_view()
            .expect("session")
            .draft
            .text_size_percent,
        130
    );
    assert!(owner.take_style_save_request().is_none());
}

#[test]
fn saved_move_lifts_the_real_cue_by_the_moved_fraction_on_the_gpu() {
    let Some(mut harness) = Harness::with_size(WIDTH, HEIGHT) else {
        eprintln!("skip: no GPU adapter for the appearance edit test");
        return;
    };
    let media = instance(1);
    let (mut owner, _directory) = ready_owner(media);
    let rect: PictureRect = full_rect(WIDTH, HEIGHT);
    let (_, video_only) = harness.frame(rect, None);
    let render = |owner: &mut SubtitleOwner, harness: &mut Harness| -> Vec<u8> {
        let placed = owner
            .placed_overlay_for_frame(
                current(media, GENERATION),
                &frame_at(INSIDE_CUE_MILLIS, GENERATION),
            )
            .expect("real cue");
        let (overlay, offset) = (placed.overlay.clone(), placed.layer_offsets.main);
        harness.frame_with_offset(rect, Some(&overlay), offset).1
    };
    let top_row = |pixels: &[u8]| -> u32 {
        (0..HEIGHT)
            .find(|&y| {
                (0..WIDTH).any(|x| {
                    let offset = ((y * WIDTH + x) * 4) as usize;
                    pixels[offset..offset + 4] != video_only[offset..offset + 4]
                })
            })
            .expect("subtitle ink")
    };
    let before = top_row(&render(&mut owner, &mut harness));

    owner.begin_appearance_edit();
    owner.apply_appearance_edit(AppearanceEdit::MoveBy {
        horizontal_fraction: 0.0,
        vertical_fraction: 0.2,
    });
    owner.finish_appearance_edit(AppearanceEditFinish::Save);
    let saved = owner.take_style_save_request().expect("queued");
    let mut committed = enabled_config();
    committed.position_vertical_percent = saved.position.vertical_percent;
    owner.apply_config(&committed);

    let after = top_row(&render(&mut owner, &mut harness));
    let expected_shift = 0.2 * HEIGHT as f32;
    assert!(
        ((before as f32 - after as f32) - expected_shift).abs() <= 1.0,
        "cue moved up by 0.2 of the picture height: rows {before} -> {after}, expected {expected_shift}"
    );
}

#[test]
fn frame_loop_hands_the_saved_style_to_one_transaction_and_reports_failure() {
    // `AppState` нельзя собрать без окна: закрепляем проводку исходником (как для жизненного цикла media).
    let frame_loop = include_str!("../../../frame_prepare.rs");
    assert!(frame_loop.contains("take_subtitle_style_commit"));
    assert!(frame_loop.contains("commit_subtitle_style"));
    assert!(frame_loop.contains("report_subtitle_style_save_failed"));
    let glue = include_str!("../../subtitle_appearance_edit.rs");
    assert!(glue.contains("Could not save subtitle style."));
}
