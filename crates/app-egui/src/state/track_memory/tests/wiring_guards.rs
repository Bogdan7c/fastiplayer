//! Охранные тесты склейки: `AppState` без окна не собрать, поэтому порядок вызовов в местах
//! подключения закреплён проверкой исходников (как `media_lifecycle_hooks_start_and_clear_subtitles`).
//! Сама логика под этими вызовами проверена настоящими владельцами в остальных тестах.

#[test]
fn audio_choice_is_remembered_only_after_the_command_was_accepted() {
    let source = include_str!("../../audio_tracks/control_actions.rs");
    let funnel = source
        .split("pub(crate) fn select_audio_track_by_user")
        .nth(1)
        .expect("funnel exists");
    let requested = funnel
        .find("AudioTrackSelectionFeedback::Requested")
        .expect("arm");
    let remember = funnel
        .find("remember_user_audio_choice")
        .expect("memory call");
    let send_failed = funnel
        .find("AudioTrackSelectionFeedback::SendFailed")
        .expect("arm");
    assert!(
        requested < remember && remember < send_failed,
        "memory is written inside the Requested arm only"
    );
}

#[test]
fn media_start_restores_memory_before_subtitles_begin_and_captures_dropped_files_after() {
    let source = include_str!("../../subtitles/app_state_api.rs");
    let start = source
        .split("fn start_subtitles_for_installed_media")
        .nth(1)
        .expect("start hook exists");
    let memory = start
        .find("begin_track_memory_for_media")
        .expect("key switch");
    let begin = start
        .find("begin_media_restoring")
        .expect("restoring begin");
    let capture_after = start[begin..].find("capture_user_subtitle_choices");
    assert!(
        memory < begin,
        "the video key is switched before subtitles start"
    );
    assert!(
        capture_after.is_some(),
        "a file dropped together with the video is captured right after the start"
    );
}

#[test]
fn every_open_path_that_restores_state_asks_memory_for_the_audio_track() {
    let strong = include_str!("../../strong_media_open/pending/resume.rs");
    assert!(
        strong.contains("remembered_audio_restore(&installed.source, &snapshot)"),
        "restore-with-position path"
    );
    assert!(
        strong.contains("select_remembered_audio_before_playback(&installed.source, &snapshot)"),
        "continue-without-position path"
    );
    let suspended = include_str!("../../suspended_media_resume.rs");
    assert!(
        suspended.contains("remembered_audio_restore(&attempt.source"),
        "suspend resume path"
    );
}

#[test]
fn user_subtitle_inputs_are_captured_right_after_they_are_applied() {
    for (name, source) in [
        (
            "control actions",
            include_str!("../../subtitle_control_actions.rs"),
        ),
        (
            "attach file",
            include_str!("../../subtitles/app_state_api.rs"),
        ),
    ] {
        assert!(
            source.contains("capture_user_subtitle_choices"),
            "{name} must hand user choices to the memory"
        );
    }
}
