//! Сохранение настроек «только тронутые ключи» через настоящий `AppConfigStore::persist`
//! и настоящую файловую систему: два процесса, занятый замок, битый/новый/отсутствующий
//! файл, ключи соседа и поведение транзакции при ошибке сохранения.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fastiplayer_config::{config_write_lock_path_for, load_from_path, save_validated_atomic_at};
use settings_core::{
    ApplyFinalState, ApplyRouteReport, CommittedApplyRequest, CommittedFinalizeRequest,
    CommittedRollbackRequest, CommittedSettingsApplier, PersistOutcome, RollbackReport, SettingId,
    SettingValue, SettingsController, SettingsPersister, SettingsValidator, ValidationReport,
    ValidationRequest,
};

use super::*;
use crate::{AppConfigStore, AppConfigValidator};

/// Переменная окружения: путь config-а для дочернего процесса-писателя.
const CHILD_CONFIG_PATH_ENV: &str = "FASTIPLAYER_TEST_SHARED_CONFIG_PATH";
/// Переменная окружения: какой набор ключей пишет дочерний процесс (`a` или `b`).
const CHILD_WRITER_ENV: &str = "FASTIPLAYER_TEST_SHARED_CONFIG_WRITER";
/// Переменная окружения: файл-сигнал «стартуйте одновременно».
const CHILD_START_FILE_ENV: &str = "FASTIPLAYER_TEST_SHARED_CONFIG_START";
/// Полное имя теста-писателя внутри тестового бинаря crate-а.
const CHILD_WRITER_TEST_NAME: &str = "shared_persist::tests::shared_config_child_writer";
/// Сколько записей подряд делает каждый процесс: достаточно, чтобы записи перемежались.
const CHILD_WRITE_ROUNDS: u16 = 40;

/// Уникальный временный каталог теста (crate без dev-зависимостей, как соседние тесты).
fn unique_test_directory(label: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "fastiplayer-shared-persist-{label}-{}-{timestamp}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("test directory created");
    directory
}

/// Diff «defaults → документ с заданными значениями» по id реестра.
fn diff_with(changes: &[(&str, SettingValue)]) -> (AppConfig, SettingsDiff) {
    let registry = app_config_registry().expect("registry builds");
    let mut document = AppConfig::default();
    for (setting_id, value) in changes {
        registry
            .set_value(&mut document, &SettingId::from(*setting_id), value.clone())
            .expect("test value is valid");
    }
    let diff = registry
        .diff(&AppConfig::default(), &document)
        .expect("diff builds");
    (document, diff)
}

/// Сохраняет набор изменений через настоящий store.
fn persist_changes(
    store: &mut AppConfigStore,
    changes: &[(&str, SettingValue)],
) -> SettingsResult<PersistReport> {
    let (document, changed_settings) = diff_with(changes);
    store.persist(PersistRequest {
        document: &document,
        changed_settings: &changed_settings,
    })
}

/// Держит замок записи config-а отдельным дескриптором — как соседний процесс.
fn hold_write_lock(config_path: &Path) -> File {
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config_write_lock_path_for(config_path))
        .expect("lock file opened");
    lock_file.lock().expect("neighbor locks");
    lock_file
}

/// Имена файлов каталога, кроме config-а и замка (резервные копии, temp).
fn extra_entries(directory: &Path) -> Vec<String> {
    fs::read_dir(directory)
        .expect("directory readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "config.toml" && name != "config.toml.lock")
        .collect()
}

/// Ключи писателя `a`: ширина сайдбара и громкость.
fn writer_a_changes(round: u16) -> Vec<(&'static str, SettingValue)> {
    vec![
        (
            "ui.sidebar.width_points",
            SettingValue::Integer(i64::from(351 + round)),
        ),
        (
            "audio.volume",
            SettingValue::Float(f64::from(round) / 100.0),
        ),
    ]
}

/// Ключи писателя `b`: лимит файлов папки и размер текста субтитров.
///
/// Значения ни в одном раунде не совпадают с defaults: diff считается от defaults, и
/// совпавшее значение просто не попало бы в запись.
fn writer_b_changes(round: u16) -> Vec<(&'static str, SettingValue)> {
    vec![
        (
            "playlist.dropped_folder_max_files",
            SettingValue::Integer(i64::from(1000 + round)),
        ),
        (
            "subtitles.text_size_percent",
            SettingValue::Integer(i64::from(120 + round)),
        ),
    ]
}

/// Дочерний процесс-писатель. Без переменных окружения (обычный прогон) ничего не делает.
#[test]
fn shared_config_child_writer() {
    let Ok(config_path) = std::env::var(CHILD_CONFIG_PATH_ENV) else {
        return;
    };
    let writer = std::env::var(CHILD_WRITER_ENV).expect("writer name");
    let start_file = std::env::var(CHILD_START_FILE_ENV).expect("start file");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !Path::new(&start_file).exists() {
        assert!(Instant::now() < deadline, "start signal never came");
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut store = AppConfigStore::new(&config_path);
    for round in 1..=CHILD_WRITE_ROUNDS {
        let changes = match writer.as_str() {
            "a" => writer_a_changes(round),
            "b" => writer_b_changes(round),
            other => panic!("unknown writer {other}"),
        };
        let report = persist_changes(&mut store, &changes).expect("child persist succeeds");
        assert_eq!(report.outcome, PersistOutcome::Persisted);
    }
}

/// Запускает тестовый бинарь повторно как процесс-писатель.
fn spawn_child_writer(config_path: &Path, writer: &str, start_file: &Path) -> std::process::Child {
    Command::new(std::env::current_exe().expect("current test binary"))
        .arg("--exact")
        .arg(CHILD_WRITER_TEST_NAME)
        .arg("--nocapture")
        .env(CHILD_CONFIG_PATH_ENV, config_path)
        .env(CHILD_WRITER_ENV, writer)
        .env(CHILD_START_FILE_ENV, start_file)
        .spawn()
        .expect("child writer spawned")
}

/// Два НАСТОЯЩИХ процесса одновременно много раз пишут РАЗНЫЕ ключи: в файле оба набора
/// последних значений, ничего не потеряно.
#[test]
fn two_processes_writing_different_keys_lose_nothing() {
    let directory = unique_test_directory("two-processes");
    let config_path = directory.join("config.toml");
    save_validated_atomic_at(&config_path, &AppConfig::default()).expect("defaults written");
    let start_file = directory.join("start");

    let mut writer_a = spawn_child_writer(&config_path, "a", &start_file);
    let mut writer_b = spawn_child_writer(&config_path, "b", &start_file);
    fs::write(&start_file, b"go").expect("start signal written");
    let status_a = writer_a.wait().expect("writer a finished");
    let status_b = writer_b.wait().expect("writer b finished");

    assert!(status_a.success(), "writer a failed: {status_a}");
    assert!(status_b.success(), "writer b failed: {status_b}");
    let on_disk = load_from_path(&config_path)
        .expect("final file loads")
        .config;
    let last_round = CHILD_WRITE_ROUNDS;
    assert_eq!(on_disk.ui.sidebar.width_points, 351 + last_round);
    assert!((on_disk.audio.volume - f64::from(last_round) / 100.0).abs() < 1e-9);
    assert_eq!(on_disk.playlist.dropped_folder_max_files, 1000 + last_round);
    assert_eq!(on_disk.subtitles.text_size_percent, 120 + last_round);
    assert!(extra_entries(&directory).iter().all(|name| name == "start"));
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Сосед поменял X на диске, мы сохраняем Y: X сохранился.
#[test]
fn persist_keeps_key_changed_by_neighbor() {
    let directory = unique_test_directory("neighbor-key");
    let config_path = directory.join("config.toml");
    let mut neighbor_config = AppConfig::default();
    neighbor_config.ui.language = "en".into();
    save_validated_atomic_at(&config_path, &neighbor_config).expect("neighbor wrote X");
    let mut store = AppConfigStore::new(&config_path);

    let report = persist_changes(
        &mut store,
        &[("ui.sidebar.width_points", SettingValue::Integer(480))],
    )
    .expect("persist succeeds");

    assert_eq!(report.outcome, PersistOutcome::Persisted);
    let on_disk = load_from_path(&config_path).expect("file loads").config;
    assert_eq!(on_disk.ui.language, "en");
    assert_eq!(on_disk.ui.sidebar.width_points, 480);
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Нет файла → создан с defaults и изменённым ключом.
#[test]
fn persist_creates_missing_file_from_defaults_plus_change() {
    let directory = unique_test_directory("missing");
    let config_path = directory.join("config.toml");
    let mut store = AppConfigStore::new(&config_path);

    persist_changes(
        &mut store,
        &[("ui.sidebar.width_points", SettingValue::Integer(480))],
    )
    .expect("persist succeeds");

    let mut expected = AppConfig::default();
    expected.ui.sidebar.width_points = 480;
    assert_eq!(
        load_from_path(&config_path).expect("file loads").config,
        expected
    );
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Замок держит сосед дольше срока → понятная ошибка «занят», файл байт-в-байт прежний.
#[test]
fn busy_lock_is_a_clear_error_and_file_is_untouched() {
    let directory = unique_test_directory("busy");
    let config_path = directory.join("config.toml");
    save_validated_atomic_at(&config_path, &AppConfig::default()).expect("defaults written");
    let original_bytes = fs::read(&config_path).expect("original bytes");
    let neighbor_lock = hold_write_lock(&config_path);
    let mut store = AppConfigStore::new(&config_path).with_lock_wait(Duration::from_millis(50));

    let error = persist_changes(
        &mut store,
        &[("ui.sidebar.width_points", SettingValue::Integer(480))],
    )
    .expect_err("busy lock refuses");

    let message = error.to_string();
    assert!(message.contains("Settings were not saved"), "{message}");
    assert!(message.contains("busy"), "{message}");
    assert_eq!(fs::read(&config_path).expect("file"), original_bytes);
    drop(neighbor_lock);
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Битый файл → ошибка, файл не изменён, `.bak` этим путём не создан.
#[test]
fn broken_file_is_a_clear_error_without_backup() {
    let directory = unique_test_directory("broken");
    let config_path = directory.join("config.toml");
    fs::write(&config_path, "schema_version = 29\n[[[").expect("broken file written");
    let mut store = AppConfigStore::new(&config_path);

    let error = persist_changes(
        &mut store,
        &[("ui.sidebar.width_points", SettingValue::Integer(480))],
    )
    .expect_err("broken file refuses");

    let message = error.to_string();
    assert!(message.contains("syntax error"), "{message}");
    assert!(message.contains("left unchanged"), "{message}");
    assert_eq!(
        fs::read_to_string(&config_path).expect("file"),
        "schema_version = 29\n[[["
    );
    assert!(extra_entries(&directory).is_empty());
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Файл новее схемы → ошибка, файл не изменён.
#[test]
fn newer_schema_file_is_a_clear_error_and_untouched() {
    let directory = unique_test_directory("newer");
    let config_path = directory.join("config.toml");
    let newer_text = AppConfig::default()
        .to_pretty_toml()
        .expect("defaults serialize")
        .replace("schema_version = 29", "schema_version = 99");
    fs::write(&config_path, &newer_text).expect("newer file written");
    let mut store = AppConfigStore::new(&config_path);

    let error = persist_changes(
        &mut store,
        &[("ui.sidebar.width_points", SettingValue::Integer(480))],
    )
    .expect_err("newer file refuses");

    assert!(error.to_string().contains("newer version"), "{error}");
    assert_eq!(fs::read_to_string(&config_path).expect("file"), newer_text);
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Ключи `[browser_session]` и `[browser_session.window]` пишутся по id реестра.
#[test]
fn browser_session_keys_are_persisted_by_setting_id() {
    let directory = unique_test_directory("browser-keys");
    let config_path = directory.join("config.toml");
    let mut store = AppConfigStore::new(&config_path);

    persist_changes(
        &mut store,
        &[
            ("browser_session.buffer_ahead_s", SettingValue::Integer(200)),
            (
                "browser_session.window.inner_width_points",
                SettingValue::Integer(1600),
            ),
            (
                "browser_session.window.inner_height_points",
                SettingValue::Integer(900),
            ),
            (
                "browser_session.window.fullscreen",
                SettingValue::Bool(true),
            ),
        ],
    )
    .expect("persist succeeds");

    let browser_session = load_from_path(&config_path)
        .expect("file loads")
        .config
        .browser_session;
    assert_eq!(browser_session.buffer_ahead_s, 200);
    assert_eq!(browser_session.window.inner_width_points, 1600);
    assert_eq!(browser_session.window.inner_height_points, 900);
    assert!(browser_session.window.fullscreen);
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Делегат транзакции: настоящие validator и store, рантайм-маршруты — заглушка со
/// счётчиком откатов (откат = рантайм вернулся к прежнему значению).
struct ProbeDelegate {
    store: AppConfigStore,
    rollback_count: usize,
}

impl SettingsValidator<AppConfig> for ProbeDelegate {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, AppConfig>,
    ) -> SettingsResult<ValidationReport> {
        AppConfigValidator.validate(request)
    }
}

impl SettingsPersister<AppConfig> for ProbeDelegate {
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        self.store.persist(request)
    }
}

impl CommittedSettingsApplier<AppConfig> for ProbeDelegate {
    fn preflight_committed(
        &mut self,
        _request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        Ok(Vec::new())
    }

    fn apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        Ok(request
            .route_updates
            .iter()
            .map(|update| {
                ApplyRouteReport::applied(update.route.clone(), update.affected_settings.clone())
            })
            .collect())
    }

    fn rollback_committed(
        &mut self,
        request: CommittedRollbackRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<RollbackReport>> {
        self.rollback_count += 1;
        Ok(request
            .route_updates
            .iter()
            .map(|update| {
                RollbackReport::rolled_back(update.route.clone(), update.affected_settings.clone())
            })
            .collect())
    }

    fn finalize_committed(&mut self, _request: CommittedFinalizeRequest<'_, AppConfig>) {}
}

/// Битый файл на диске при Apply: сохранение отказывает, рантайм откатан, committed
/// прежний, а черновик с изменением жив — изменение не потеряно молча.
#[test]
fn apply_with_broken_disk_file_rolls_back_runtime_and_keeps_draft() {
    let directory = unique_test_directory("apply-broken");
    let config_path = directory.join("config.toml");
    fs::write(&config_path, "[[[").expect("broken file written");
    let mut controller = SettingsController::new(
        AppConfig::default(),
        app_config_registry().expect("registry"),
    );
    controller
        .set_value(
            SettingId::from("ui.sidebar.width_points"),
            SettingValue::Integer(480),
        )
        .expect("draft edit succeeds");
    let mut delegate = ProbeDelegate {
        store: AppConfigStore::new(&config_path),
        rollback_count: 0,
    };

    let report = controller
        .apply(&mut delegate)
        .expect("apply returns report");

    assert_eq!(report.final_state, ApplyFinalState::PersistFailed);
    assert_eq!(delegate.rollback_count, 1);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.to_string().contains("Settings were not saved"))
    );
    assert_eq!(controller.committed().ui.sidebar.width_points, 420);
    assert_eq!(controller.draft().ui.sidebar.width_points, 480);
    assert_eq!(fs::read_to_string(&config_path).expect("file"), "[[[");
    fs::remove_dir_all(&directory).expect("test directory removed");
}
