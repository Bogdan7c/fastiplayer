//! Запоминание размера и фуллскрина окна браузерного режима: пауза, латест-онли,
//! «в фуллскрине размер не пишется», запись только ключей `browser_session.window.*`.

use crate::settings_runtime::browser_window_geometry::BrowserWindowGeometryFlushOutcome;
use crate::settings_runtime::{BrowserWindowObservation, WindowedSizePoints};

use super::*;

/// Окно такого размера в логических точках.
fn windowed(width: u32, height: u32) -> BrowserWindowObservation {
    BrowserWindowObservation::Windowed(WindowedSizePoints::clamped(width, height))
}

/// Среда теста: runtime над настоящим файлом и записывающий adapter.
struct GeometryHarness {
    runtime: SettingsRuntime,
    adapter: RecordingRuntimeAdapter,
    path: PathBuf,
}

impl GeometryHarness {
    /// Runtime над config-ом `config`, записанным в уникальный файл теста.
    fn over_file(test_name: &str, config: AppConfig) -> Self {
        let path = temp_config_path(test_name);
        remove_file_if_exists(&path);
        fastiplayer_config::save_validated_atomic_at(&path, &config).expect("seed config file");
        let runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
            config.clone(),
            path.clone(),
        ))
        .expect("settings runtime should build");
        let adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter should build");
        Self {
            runtime,
            adapter,
            path,
        }
    }

    /// Config так, как он лежит на диске сейчас.
    fn on_disk(&self) -> AppConfig {
        fastiplayer_config::load_from_path(&self.path)
            .expect("persisted config should reload")
            .config
    }
}

impl Drop for GeometryHarness {
    fn drop(&mut self) {
        remove_file_if_exists(&self.path);
    }
}

/// Пауза склеивает изменения: срок двигается только отличающимся значением, ранний тик
/// ничего не пишет, тик в срок пишет ПОСЛЕДНЕЕ значение в файл.
#[test]
fn resize_waits_for_quiet_and_persists_the_latest_size() {
    let mut harness = GeometryHarness::over_file("browser-geometry-debounce", AppConfig::default());
    let started_at = Instant::now();

    assert!(
        harness
            .runtime
            .record_browser_window_observation(windowed(900, 500), started_at)
    );
    let first_deadline = harness
        .runtime
        .next_browser_window_geometry_deadline()
        .expect("resize schedules a deadline");
    assert_eq!(first_deadline, started_at + Duration::from_millis(500));
    assert!(
        !harness.runtime.record_browser_window_observation(
            windowed(900, 500),
            started_at + Duration::from_millis(100)
        ),
        "the same size must not move the deadline"
    );
    assert_eq!(
        harness.runtime.next_browser_window_geometry_deadline(),
        Some(first_deadline)
    );

    let replacement_at = started_at + Duration::from_millis(200);
    assert!(
        harness
            .runtime
            .record_browser_window_observation(windowed(910, 510), replacement_at)
    );
    let replacement_deadline = replacement_at + Duration::from_millis(500);
    assert_eq!(
        harness
            .runtime
            .flush_due_browser_window_geometry(
                replacement_deadline - Duration::from_millis(1),
                &mut harness.adapter
            )
            .expect("early tick stays pending"),
        BrowserWindowGeometryFlushOutcome::NoPending
    );
    assert_eq!(
        harness.on_disk().browser_session.window.inner_width_points,
        1280
    );

    assert_eq!(
        harness
            .runtime
            .flush_due_browser_window_geometry(replacement_deadline, &mut harness.adapter)
            .expect("tick at the deadline commits"),
        BrowserWindowGeometryFlushOutcome::Succeeded
    );
    let window = harness.on_disk().browser_session.window;
    assert_eq!(
        (
            window.inner_width_points,
            window.inner_height_points,
            window.fullscreen
        ),
        (910, 510, false)
    );
    assert_eq!(
        harness.runtime.next_browser_window_geometry_deadline(),
        None
    );
}

/// Вход в фуллскрин пишет флаг и сохраняет последний оконный размер (не размер экрана).
#[test]
fn entering_fullscreen_writes_the_flag_and_keeps_the_windowed_size() {
    let mut harness = GeometryHarness::over_file("browser-geometry-enter-fs", AppConfig::default());
    let now = Instant::now();

    harness
        .runtime
        .record_browser_window_observation(windowed(900, 500), now);
    harness
        .runtime
        .record_browser_window_observation(BrowserWindowObservation::Fullscreen, now);
    harness
        .runtime
        .flush_pending_browser_window_geometry(&mut harness.adapter)
        .expect("forced flush commits");

    let window = harness.on_disk().browser_session.window;
    assert_eq!(
        (
            window.inner_width_points,
            window.inner_height_points,
            window.fullscreen
        ),
        (900, 500, true)
    );
}

/// Окно уже запомнено в фуллскрине: наблюдения в фуллскрине (размер экрана при каждом
/// ресайзе) ничего не планируют и ничего не пишут.
#[test]
fn size_changes_while_in_fullscreen_are_not_recorded() {
    let mut config = AppConfig::default();
    config.browser_session.window.inner_width_points = 900;
    config.browser_session.window.inner_height_points = 500;
    config.browser_session.window.fullscreen = true;
    let mut harness = GeometryHarness::over_file("browser-geometry-in-fs", config);

    assert!(
        !harness.runtime.record_browser_window_observation(
            BrowserWindowObservation::Fullscreen,
            Instant::now()
        )
    );
    assert_eq!(
        harness.runtime.next_browser_window_geometry_deadline(),
        None
    );
    assert_eq!(
        harness
            .runtime
            .flush_pending_browser_window_geometry(&mut harness.adapter)
            .expect("nothing to flush"),
        BrowserWindowGeometryFlushOutcome::NoPending
    );
    let window = harness.on_disk().browser_session.window;
    assert_eq!(
        (window.inner_width_points, window.inner_height_points),
        (900, 500)
    );
}

/// Выход из фуллскрина: новое оконное наблюдение сбрасывает флаг и пишет новый размер.
#[test]
fn leaving_fullscreen_clears_the_flag_and_writes_the_new_size() {
    let mut config = AppConfig::default();
    config.browser_session.window.fullscreen = true;
    let mut harness = GeometryHarness::over_file("browser-geometry-leave-fs", config);

    harness
        .runtime
        .record_browser_window_observation(windowed(1000, 600), Instant::now());
    harness
        .runtime
        .flush_pending_browser_window_geometry(&mut harness.adapter)
        .expect("forced flush commits");

    let window = harness.on_disk().browser_session.window;
    assert_eq!(
        (
            window.inner_width_points,
            window.inner_height_points,
            window.fullscreen
        ),
        (1000, 600, false)
    );
}

/// Запись трогает только `browser_session.window.*`: чужие изменения на диске (другая
/// копия плеера) не теряются, остальные секции остаются как были.
#[test]
fn geometry_commit_touches_only_the_window_keys_and_keeps_neighbour_changes() {
    let mut harness = GeometryHarness::over_file("browser-geometry-keys", AppConfig::default());
    // Другая копия плеера успела записать громкость, пока мы были открыты.
    let mut neighbour = AppConfig::default();
    neighbour.audio.volume = 0.35;
    fastiplayer_config::save_validated_atomic_at(&harness.path, &neighbour)
        .expect("neighbour write");

    harness
        .runtime
        .record_browser_window_observation(windowed(900, 500), Instant::now());
    harness
        .runtime
        .flush_pending_browser_window_geometry(&mut harness.adapter)
        .expect("forced flush commits");

    let on_disk = harness.on_disk();
    assert!((on_disk.audio.volume - 0.35).abs() < f64::EPSILON);
    assert_eq!(on_disk.browser_session.window.inner_width_points, 900);
    // Всё, кроме окна браузерного режима, совпадает с тем, что записал сосед.
    let mut expected = neighbour;
    expected.browser_session.window = on_disk.browser_session.window;
    assert_eq!(on_disk, expected);
}

/// Размер вне пределов схемы прижимается к ним, а не портит файл.
#[test]
fn out_of_range_sizes_are_clamped_to_the_schema_limits() {
    let mut harness = GeometryHarness::over_file("browser-geometry-clamp", AppConfig::default());

    harness
        .runtime
        .record_browser_window_observation(windowed(10, 99_999), Instant::now());
    harness
        .runtime
        .flush_pending_browser_window_geometry(&mut harness.adapter)
        .expect("forced flush commits");

    let window = harness.on_disk().browser_session.window;
    assert_eq!(
        (window.inner_width_points, window.inner_height_points),
        (
            fastiplayer_config::MIN_BROWSER_WINDOW_WIDTH_POINTS,
            fastiplayer_config::MAX_BROWSER_WINDOW_HEIGHT_POINTS
        )
    );
}

/// Файл не позволяет писать (битый, чинит обычный плеер): значение действует в памяти,
/// файл на диске остаётся прежним байт-в-байт.
#[test]
fn geometry_over_a_session_only_config_stays_in_memory() {
    let path = temp_config_path("browser-geometry-session-only");
    remove_file_if_exists(&path);
    let broken = b"[audio\nvolume = \n";
    fs::write(&path, broken).expect("write broken file");
    let mut runtime = SettingsRuntime::from_loaded_config(LoadedConfig {
        config: AppConfig::default(),
        path: path.clone(),
        origin: ConfigLoadOrigin::DefaultsInMemory,
        save_policy: ConfigSavePolicy::SessionOnly(
            fastiplayer_config::ConfigSessionOnlyReason::BrokenConfigLeftToRegularPlayer(
                fastiplayer_config::BrokenConfigProblem::InvalidTomlSyntax,
            ),
        ),
    })
    .expect("settings runtime should build");
    let mut adapter = RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter");

    runtime.record_browser_window_observation(windowed(900, 500), Instant::now());
    runtime
        .flush_pending_browser_window_geometry(&mut adapter)
        .expect("session-only flush succeeds");

    assert_eq!(
        runtime
            .committed_config()
            .browser_session
            .window
            .inner_width_points,
        900
    );
    assert_eq!(fs::read(&path).expect("read back"), broken);
    remove_file_if_exists(&path);
}
