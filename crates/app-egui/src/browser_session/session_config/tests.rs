//! Чтение config.toml браузерным режимом: только чтение, битый файл → defaults.

use std::fs;
use std::time::Duration;

use fastiplayer_config::AppConfig;

use super::*;

/// Каталог «config пользователя» во временной папке.
fn config_directory() -> tempfile::TempDir {
    tempfile::tempdir().expect("temporary config directory")
}

/// Отсутствующий файл → значения по умолчанию, предупреждения нет, файл НЕ создан.
#[test]
fn missing_config_gives_defaults_and_creates_nothing() {
    let directory = config_directory();
    let config_file = directory.path().join("config.toml");

    let settings = BrowserSessionSettings::load_read_only_from(&config_file);

    assert_eq!(
        settings.origin(),
        BrowserSettingsOrigin::DefaultsWithoutFile
    );
    assert_eq!(settings.app_config(), &AppConfig::default());
    assert!(
        !config_file.exists(),
        "the browser process must not create config.toml"
    );
    let leftovers: Vec<_> = fs::read_dir(directory.path())
        .expect("read config directory")
        .collect();
    assert!(
        leftovers.is_empty(),
        "nothing may be written next to it: {leftovers:?}"
    );
}

/// Не-TOML → defaults + предупреждение «не та схема», байты файла не тронуты.
#[test]
fn unparsable_config_gives_defaults_with_warning_and_stays_untouched() {
    let directory = config_directory();
    let config_file = directory.path().join("config.toml");
    let broken_bytes = b"[audio\noutput_device = \"usb\"\n".to_vec();
    fs::write(&config_file, &broken_bytes).expect("write broken config");

    let settings = BrowserSessionSettings::load_read_only_from(&config_file);

    assert_eq!(
        settings.origin(),
        BrowserSettingsOrigin::DefaultsAfterBrokenConfig(BrokenConfigReason::NotMatchingSchema)
    );
    assert_eq!(settings.app_config(), &AppConfig::default());
    assert_eq!(fs::read(&config_file).expect("read back"), broken_bytes);
    let entries = fs::read_dir(directory.path()).expect("read dir").count();
    assert_eq!(entries, 1, "no .bak or temp file may appear");
}

/// Значение вне диапазона → defaults + предупреждение «неверное значение», файл цел.
#[test]
fn out_of_range_value_gives_defaults_with_invalid_value_warning() {
    let directory = config_directory();
    let config_file = directory.path().join("config.toml");
    let mut config = AppConfig::default();
    // Запас вперёд больше допустимого максимума (600 с).
    config.browser_session.buffer_ahead_s = 9_000;
    // `to_pretty_toml` не проверяет значения — так и получается «битый» файл.
    let text = config.to_pretty_toml().expect("serialize config");
    fs::write(&config_file, text.as_bytes()).expect("write config");

    let settings = BrowserSessionSettings::load_read_only_from(&config_file);

    assert_eq!(
        settings.origin(),
        BrowserSettingsOrigin::DefaultsAfterBrokenConfig(BrokenConfigReason::InvalidValue)
    );
    assert_eq!(
        settings.buffer_window().ahead(),
        Duration::from_secs(120),
        "the broken value must not leak through"
    );
    assert_eq!(fs::read(&config_file).expect("read back"), text.as_bytes());
}

/// Корректный файл → значения из него: окно буфера и устройство звука.
#[test]
fn valid_config_values_are_read() {
    let directory = config_directory();
    let config_file = directory.path().join("config.toml");
    let mut config = AppConfig::default();
    config.browser_session.buffer_ahead_s = 90;
    config.browser_session.buffer_behind_s = 15;
    config.audio.output_device = "usb-headset".to_string();
    config.video.sw_decode_threads = 3;
    let text = config.to_pretty_toml().expect("serialize config");
    fs::write(&config_file, text.as_bytes()).expect("write config");

    let settings = BrowserSessionSettings::load_read_only_from(&config_file);

    assert_eq!(settings.origin(), BrowserSettingsOrigin::ConfigFile);
    assert_eq!(settings.buffer_window().ahead(), Duration::from_secs(90));
    assert_eq!(settings.buffer_window().behind(), Duration::from_secs(15));
    assert_eq!(settings.audio_output_device_id(), "usb-headset");
    assert_eq!(settings.app_config().video.sw_decode_threads, 3);
    assert_eq!(fs::read(&config_file).expect("read back"), text.as_bytes());
}

/// Значения по умолчанию совпадают с решением владельца: 120 с вперёд, 30 с назад.
#[test]
fn defaults_hold_the_owner_buffer_window() {
    let settings = BrowserSessionSettings::defaults();

    assert_eq!(settings.buffer_window().ahead(), Duration::from_secs(120));
    assert_eq!(settings.buffer_window().behind(), Duration::from_secs(30));
    assert_eq!(settings.audio_output_device_id(), "default");
}

/// Метки причин стабильны: по ним e2e и разбор журнала узнают поломку.
#[test]
fn broken_reason_labels_are_stable() {
    assert_eq!(
        BrokenConfigReason::NotMatchingSchema.log_label(),
        "not_matching_schema"
    );
    assert_eq!(
        BrokenConfigReason::InvalidValue.log_label(),
        "invalid_value"
    );
    assert_eq!(BrokenConfigReason::Unreadable.log_label(), "unreadable");
    assert_eq!(
        BrokenConfigReason::ConfigDirectoryUnknown.log_label(),
        "config_directory_unknown"
    );
}

/// Каталог вместо файла → «не читается», а не «нет файла».
#[test]
fn directory_instead_of_file_is_reported_as_unreadable() {
    let directory = config_directory();
    let config_file = directory.path().join("config.toml");
    fs::create_dir(&config_file).expect("create directory in place of the file");

    let settings = BrowserSessionSettings::load_read_only_from(&config_file);

    assert_eq!(
        settings.origin(),
        BrowserSettingsOrigin::DefaultsAfterBrokenConfig(BrokenConfigReason::Unreadable)
    );
}

/// Настройки файла берутся из `[network]` пользователя: окно предзагрузки и бюджет
/// повторов доходят до пределов источника без второго перевода настроек.
#[test]
fn file_source_follows_the_network_section() {
    let directory = config_directory();
    let config_file = directory.path().join("config.toml");
    let mut config = AppConfig::default();
    config.network.read_ahead_mb = 64;
    config.network.prefetch_chunk_mb = 4;
    config.network.prefetch_initial_chunk_kb = 256;
    config.network.reconnect_wait_ms = 7_000;
    let text = config.to_pretty_toml().expect("serialize config");
    fs::write(&config_file, text.as_bytes()).expect("write config");

    let settings = BrowserSessionSettings::load_read_only_from(&config_file);
    let file_source = settings.file_source().expect("file source settings");
    let policy = file_source.policy().expect("file source policy");

    assert_eq!(file_source.prefetch().window_bytes(), 64 * 1024 * 1024);
    assert_eq!(file_source.prefetch().chunk_bytes(), 4 * 1024 * 1024);
    assert_eq!(file_source.prefetch().initial_chunk_bytes(), 256 * 1024);
    assert_eq!(file_source.reconnect_budget(), Duration::from_secs(7));
    assert_eq!(policy.prefetch(), file_source.prefetch());
    assert_eq!(policy.retry().budget(), Duration::from_secs(7));
    assert_eq!(
        policy.max_request_bytes().get(),
        4 * 1024 * 1024,
        "one range request is one prefetch chunk"
    );
}

/// Без config-а файл читается по умолчаниям `[network]` (как обычная ссылка), а
/// `reconnect_wait_ms = 0` честно выключает повторы.
#[test]
fn file_source_defaults_and_disabled_retries() {
    let defaults = BrowserSessionSettings::defaults()
        .file_source()
        .expect("default file source settings");
    assert_eq!(defaults.reconnect_budget(), Duration::from_secs(60));
    assert_eq!(defaults.prefetch().chunk_bytes(), 8 * 1024 * 1024);

    let mut config = AppConfig::default();
    config.network.reconnect_wait_ms = 0;
    let no_retries = BrowserSessionSettings::from_app_config_for_test(config)
        .file_source()
        .expect("settings without retries");
    assert_eq!(
        no_retries.policy().expect("policy").retry().budget(),
        Duration::ZERO
    );
}
