//! Действия UI (кнопка CC, меню, клавиши C/V/Z/X) через `SubtitleOwner::apply_control_action`
//! на настоящих файлах и фоновой загрузке: результат проверяется по тому, что увидит кадр
//! (overlay), и по тексту уведомления.

use subtitle_core::SubtitleSyncOffset;

use super::super::control_actions::ControlActionFeedback;
use super::super::intents::SubtitleVisibility;
use super::super::sync_offset::SyncNudge;
use super::*;
use crate::state::notifications::TransientReplaceKey;
use crate::ui::player_controls::SubtitleControlAction;

const GENERATION: u64 = 3;
const VIEW: (u32, u32) = (1280, 720);

/// Английская дорожка: реплики [1 с, 3 с) и [5 с, 7 с).
const ENGLISH_SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nEnglish line\n\n\
2\n00:00:05,000 --> 00:00:07,000\nEnglish again\n";
/// Русская дорожка: одна реплика [2 с, 4 с).
const RUSSIAN_SRT: &str = "1\n00:00:02,000 --> 00:00:04,000\nРусская строка\n";
/// Одна реплика, начинающаяся ровно на 10,0 с.
const LATE_CUE_SRT: &str = "1\n00:00:10,000 --> 00:00:12,000\nLate cue\n";

fn movie_with(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    for (name, content) in files {
        std::fs::write(directory.path().join(name), content).expect("write subtitle file");
    }
    (directory, video)
}

fn config(enabled: bool, languages: &str) -> SubtitlesConfig {
    SubtitlesConfig {
        enabled,
        preferred_languages: languages.to_owned(),
        ..SubtitlesConfig::default()
    }
}

/// Есть ли overlay на кадре с `pts_millis`.
fn overlay_present(owner: &mut SubtitleOwner, media: MediaInstanceId, pts_millis: u64) -> bool {
    owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(pts_millis, GENERATION, full_rect(VIEW.0, VIEW.1)),
        )
        .is_some()
}

fn replacing(key: TransientReplaceKey, text: &str) -> ControlActionFeedback {
    ControlActionFeedback::ReplacingToast {
        key,
        text: text.to_owned(),
    }
}

#[test]
fn cc_toggle_hides_then_shows_the_overlay_of_the_same_frame() {
    let media = instance(1);
    let (_dir, video) = movie_with(&[("Movie.srt", ENGLISH_SRT)]);
    let mut owner = owner_with_config(&config(true, "en"));
    begin_and_wait(&mut owner, media, &video);
    assert!(overlay_present(&mut owner, media, 2_000), "on at the start");

    let feedback = owner.apply_control_action(SubtitleControlAction::ToggleVisibility);
    assert_eq!(feedback, ControlActionFeedback::Silent);
    assert!(!overlay_present(&mut owner, media, 2_000), "hidden by CC");
    assert_eq!(
        owner.take_enabled_preference_change(),
        Some(SubtitleVisibility::Hidden),
        "the choice is remembered for the config"
    );

    owner.apply_control_action(SubtitleControlAction::ToggleVisibility);
    assert!(overlay_present(&mut owner, media, 2_000), "shown again");
    assert_eq!(
        owner.take_enabled_preference_change(),
        Some(SubtitleVisibility::Shown)
    );
}

#[test]
fn off_item_hides_subtitles_and_a_track_click_shows_them_again() {
    let media = instance(1);
    let (_dir, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT), ("Movie.ru.srt", RUSSIAN_SRT)]);
    let mut owner = owner_with_config(&config(true, "en"));
    begin_and_wait(&mut owner, media, &video);
    let russian = owner
        .menu_view()
        .tracks
        .iter()
        .find(|track| track.label.file_name.as_deref() == Some("Movie.ru.srt"))
        .expect("russian track")
        .id;

    owner.apply_control_action(SubtitleControlAction::Disable);
    assert_eq!(owner.menu_view().visibility, SubtitleVisibility::Hidden);
    assert!(!overlay_present(&mut owner, media, 2_500));

    // Клик по дорожке выбирает её И включает показ; реплика русской дорожки на 2.5 с.
    let feedback = owner.apply_control_action(SubtitleControlAction::SelectTrack(russian));
    assert_eq!(feedback, ControlActionFeedback::Silent);
    settle_jobs(&mut owner);
    assert_eq!(owner.menu_view().visibility, SubtitleVisibility::Shown);
    assert!(overlay_present(&mut owner, media, 2_500));
}

/// Ждёт, пока фоновая загрузка выбранной дорожки закончится.
fn settle_jobs(owner: &mut SubtitleOwner) {
    let deadline = Instant::now() + LOAD_TIMEOUT;
    while owner
        .media
        .as_ref()
        .is_some_and(super::super::media_state::MediaSubtitles::has_running_jobs)
    {
        assert!(Instant::now() < deadline, "subtitle jobs did not settle");
        owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn next_track_action_cycles_with_a_replacing_toast_naming_the_track() {
    let media = instance(1);
    let (_dir, video) = movie_with(&[("Movie.en.srt", ENGLISH_SRT), ("Movie.ru.srt", RUSSIAN_SRT)]);
    let mut owner = owner_with_config(&config(true, "ru"));
    begin_and_wait(&mut owner, media, &video);
    let next = SubtitleControlAction::SelectNextTrack;

    // Выбрана последняя (русская): следующий шаг — «выкл».
    assert_eq!(
        owner.apply_control_action(next),
        replacing(TransientReplaceKey::SubtitleTrack, "Subtitles: off")
    );
    assert_eq!(
        owner.apply_control_action(next),
        replacing(
            TransientReplaceKey::SubtitleTrack,
            "Subtitles: English (SRT, file)"
        )
    );
    assert_eq!(
        owner.apply_control_action(next),
        replacing(
            TransientReplaceKey::SubtitleTrack,
            "Subtitles: Russian (SRT, file)"
        )
    );
}

#[test]
fn delay_actions_shift_the_cue_and_report_the_exact_value() {
    let media = instance(1);
    let (_dir, video) = movie_with(&[("Movie.srt", LATE_CUE_SRT)]);
    let mut owner = owner_with_config(&config(true, "en"));
    begin_and_wait(&mut owner, media, &video);
    assert!(overlay_present(&mut owner, media, 10_050));

    let later = SubtitleControlAction::NudgeSync(SyncNudge::Later);
    for _ in 0..2 {
        owner.apply_control_action(later);
    }
    let feedback = owner.apply_control_action(later);
    assert_eq!(
        feedback,
        replacing(TransientReplaceKey::SubtitleSync, "Subtitle delay: +0.3 s")
    );
    assert!(
        !overlay_present(&mut owner, media, 10_250),
        "cue is late now"
    );
    assert!(
        overlay_present(&mut owner, media, 10_350),
        "cue started at +0.3 s"
    );
    assert_eq!(
        owner.menu_view().sync_offset,
        SubtitleSyncOffset::from_millis(300)
    );

    let earlier = owner.apply_control_action(SubtitleControlAction::NudgeSync(SyncNudge::Earlier));
    assert_eq!(
        earlier,
        replacing(TransientReplaceKey::SubtitleSync, "Subtitle delay: +0.2 s")
    );
    let reset = owner.apply_control_action(SubtitleControlAction::ResetSync);
    assert_eq!(
        reset,
        replacing(TransientReplaceKey::SubtitleSync, "Subtitle delay: 0.0 s")
    );
    assert!(overlay_present(&mut owner, media, 10_050), "back in sync");

    let below = owner.apply_control_action(SubtitleControlAction::NudgeSync(SyncNudge::Earlier));
    assert_eq!(
        below,
        replacing(TransientReplaceKey::SubtitleSync, "Subtitle delay: -0.1 s")
    );
}

#[test]
fn without_tracks_keys_only_explain_and_change_nothing() {
    let mut owner = owner_with_config(&config(true, "en"));
    owner.begin_media(instance(1), SubtitleMediaSource::NotLocal);
    let expected = ControlActionFeedback::Toast("No subtitles for this video".to_owned());
    for action in [
        SubtitleControlAction::ToggleVisibility,
        SubtitleControlAction::SelectNextTrack,
        SubtitleControlAction::NudgeSync(SyncNudge::Later),
        SubtitleControlAction::NudgeSync(SyncNudge::Earlier),
        SubtitleControlAction::ResetSync,
    ] {
        assert_eq!(owner.apply_control_action(action), expected, "{action:?}");
    }
    assert_eq!(owner.menu_view().visibility, SubtitleVisibility::Shown);
    assert_eq!(owner.menu_view().sync_offset, SubtitleSyncOffset::ZERO);
    assert_eq!(
        owner.take_enabled_preference_change(),
        None,
        "pressing C with no tracks must not rewrite the remembered choice"
    );

    // Без открытого media — то же самое.
    let mut idle = owner_with_config(&config(true, "en"));
    assert_eq!(
        idle.apply_control_action(SubtitleControlAction::ToggleVisibility),
        expected
    );
}

#[test]
fn unknown_and_unavailable_tracks_are_explained_or_ignored_without_state_change() {
    let media = instance(1);
    let (_dir, video) = movie_with(&[("Movie.srt", ENGLISH_SRT), ("Movie.ass", "[Script Info]\n")]);
    let mut owner = owner_with_config(&config(true, "en"));
    begin_and_wait(&mut owner, media, &video);
    let before = owner.menu_view();
    let ass = before
        .tracks
        .iter()
        .find(|track| track.label.file_name.as_deref() == Some("Movie.ass"))
        .expect("ass track is listed")
        .id;

    assert_eq!(
        owner.apply_control_action(SubtitleControlAction::SelectTrack(ass)),
        ControlActionFeedback::Toast("This subtitle file cannot be used.".to_owned())
    );
    assert_eq!(owner.menu_view(), before, "selection is unchanged");

    let stale = subtitle_core::SubtitleTrackId::new(9_999);
    assert_eq!(
        owner.apply_control_action(SubtitleControlAction::SelectTrack(stale)),
        ControlActionFeedback::Silent
    );
    assert_eq!(owner.menu_view(), before);
}

#[test]
fn opening_the_file_dialog_is_not_a_state_change() {
    let media = instance(1);
    let (_dir, video) = movie_with(&[("Movie.srt", ENGLISH_SRT)]);
    let mut owner = owner_with_config(&config(true, "en"));
    begin_and_wait(&mut owner, media, &video);
    let before = owner.menu_view();
    assert_eq!(
        owner.apply_control_action(SubtitleControlAction::OpenLoadFileDialog),
        ControlActionFeedback::Silent
    );
    assert_eq!(owner.menu_view(), before);
}
