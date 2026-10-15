//! Запоминание «субтитры были включены» между запусками.
//!
//! «Перезапуск» настоящий: config пишется на диск через production store и читается заново.

use fastiplayer_config::load_from_path;

use super::*;
use crate::state::SubtitleVisibility;

/// Пауза перед записью в production (держим тест в синхроне с модулем).
const PERSIST_DEBOUNCE: Duration = Duration::from_secs(3);

fn runtime_at(path: &std::path::Path) -> SettingsRuntime {
    SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        AppConfig::default(),
        path.to_path_buf(),
    ))
    .expect("settings runtime should build")
}

fn adapter() -> RecordingRuntimeAdapter {
    RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter builds")
}

/// Значение, противоположное умолчанию: запись точно должна его изменить.
fn opposite_of_default() -> SubtitleVisibility {
    SubtitleVisibility::from_enabled(!AppConfig::default().subtitles.enabled)
}

fn persisted_enabled(path: &std::path::Path) -> bool {
    load_from_path(path)
        .expect("persisted config should reload")
        .config
        .subtitles
        .enabled
}

#[test]
fn choice_is_written_only_after_the_quiet_period() {
    let path = temp_config_path("subtitle-enabled-debounce");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();
    let started_at = Instant::now();

    assert!(runtime.record_subtitle_enabled_change(Some(opposite_of_default()), started_at));
    let deadline = started_at + PERSIST_DEBOUNCE;
    assert_eq!(
        runtime.next_subtitle_enabled_persist_deadline(),
        Some(deadline)
    );

    assert_eq!(
        runtime
            .flush_due_subtitle_enabled(deadline - Duration::from_millis(1), &mut adapter)
            .expect("early flush stays pending"),
        super::super::SubtitleEnabledFlushOutcome::NoPending
    );
    assert!(!path.exists(), "nothing is written before the pause ends");

    assert_eq!(
        runtime
            .flush_due_subtitle_enabled(deadline, &mut adapter)
            .expect("due flush commits"),
        super::super::SubtitleEnabledFlushOutcome::Succeeded
    );
    assert_eq!(runtime.next_subtitle_enabled_persist_deadline(), None);
    assert_eq!(
        persisted_enabled(&path),
        opposite_of_default().is_enabled(),
        "the restarted app sees the choice"
    );
    remove_file_if_exists(&path);
}

#[test]
fn pending_choice_is_flushed_on_exit_without_waiting() {
    let path = temp_config_path("subtitle-enabled-exit");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();

    runtime.record_subtitle_enabled_change(Some(opposite_of_default()), Instant::now());
    assert_eq!(
        runtime
            .flush_pending_subtitle_enabled(&mut adapter)
            .expect("lifecycle flush commits"),
        super::super::SubtitleEnabledFlushOutcome::Succeeded
    );
    assert_eq!(persisted_enabled(&path), opposite_of_default().is_enabled());
    remove_file_if_exists(&path);
}

#[test]
fn no_change_means_no_deadline_and_no_write() {
    let path = temp_config_path("subtitle-enabled-no-change");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();
    let now = Instant::now();

    assert!(!runtime.record_subtitle_enabled_change(None, now));
    assert_eq!(runtime.next_subtitle_enabled_persist_deadline(), None);
    assert_eq!(
        runtime
            .flush_due_subtitle_enabled(now + PERSIST_DEBOUNCE, &mut adapter)
            .expect("nothing to flush"),
        super::super::SubtitleEnabledFlushOutcome::NoPending
    );
    assert_eq!(
        runtime
            .flush_pending_subtitle_enabled(&mut adapter)
            .expect("nothing to flush"),
        super::super::SubtitleEnabledFlushOutcome::NoPending
    );
    assert!(
        !path.exists(),
        "no change must not create or touch the file"
    );
}

#[test]
fn toggling_back_to_the_saved_value_cancels_the_pending_write() {
    let path = temp_config_path("subtitle-enabled-toggle-back");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();
    let now = Instant::now();

    runtime.record_subtitle_enabled_change(Some(opposite_of_default()), now);
    let default_visibility =
        SubtitleVisibility::from_enabled(AppConfig::default().subtitles.enabled);
    assert!(runtime.record_subtitle_enabled_change(Some(default_visibility), now));
    assert_eq!(runtime.next_subtitle_enabled_persist_deadline(), None);
    assert_eq!(
        runtime
            .flush_pending_subtitle_enabled(&mut adapter)
            .expect("nothing pending"),
        super::super::SubtitleEnabledFlushOutcome::NoPending
    );
    assert!(!path.exists());
}

#[test]
fn burst_of_toggles_writes_the_latest_choice_once() {
    let path = temp_config_path("subtitle-enabled-burst");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();
    let started_at = Instant::now();

    // Пять нажатий C подряд: итог — противоположное умолчанию (нечётное число переключений).
    let mut visibility = SubtitleVisibility::from_enabled(AppConfig::default().subtitles.enabled);
    let mut last_press_at = started_at;
    for press_index in 0..5_u64 {
        visibility = visibility.toggled();
        last_press_at = started_at + Duration::from_millis(100 * press_index);
        runtime.record_subtitle_enabled_change(Some(visibility), last_press_at);
    }
    // Пауза отсчитывается от последнего нажатия.
    assert_eq!(
        runtime.next_subtitle_enabled_persist_deadline(),
        Some(last_press_at + PERSIST_DEBOUNCE)
    );
    runtime
        .flush_due_subtitle_enabled(last_press_at + PERSIST_DEBOUNCE, &mut adapter)
        .expect("flush commits");
    assert_eq!(persisted_enabled(&path), visibility.is_enabled());
    remove_file_if_exists(&path);
}
