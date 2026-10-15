use super::*;

/// Каждая работа по открытию в одиночку означает «ещё не конец»: файлы субтитров держим.
#[test]
fn any_single_open_activity_keeps_the_wait_alive() {
    let busy_cases = [
        OpenActivity {
            local_file_open: true,
            ..OpenActivity::default()
        },
        OpenActivity {
            strong_open_slot: true,
            ..OpenActivity::default()
        },
        OpenActivity {
            playlist_transport: true,
            ..OpenActivity::default()
        },
        OpenActivity {
            confirmation_dialog: true,
            ..OpenActivity::default()
        },
        OpenActivity {
            folder_walk: true,
            ..OpenActivity::default()
        },
        OpenActivity {
            playlist_import: true,
            ..OpenActivity::default()
        },
        OpenActivity {
            url_import: true,
            ..OpenActivity::default()
        },
        OpenActivity {
            media_reset: true,
            ..OpenActivity::default()
        },
    ];
    for activity in busy_cases {
        assert!(!activity.is_idle(), "{activity:?} must keep waiting");
    }
}

#[test]
fn no_activity_means_the_open_is_over() {
    assert!(OpenActivity::default().is_idle());
}

/// Охранный тест проводки: без вызова в опросе открытий файлы прилипали бы к чужому видео.
/// `AppState` без окна не собрать, поэтому закрепляем проверкой исходника.
#[test]
fn open_poll_settles_dropped_subtitles_after_polling() {
    let source = include_str!("../media_jobs.rs");
    let start = source
        .find("pub fn poll_local_file_open_job")
        .expect("poll function exists");
    let body = &source[start..];
    let settle = body
        .find("settle_dropped_subtitles")
        .expect("settle is wired");
    let drain = body
        .find("drain_local_file_open_job")
        .expect("drain is called");
    assert!(
        settle > drain,
        "settle must run after the open work was polled"
    );
}
