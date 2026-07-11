//! Неграфический helper для generated config локальных smoke-сценариев.

use std::{env, path::PathBuf, process::ExitCode};

use rustiplayer_config::{
    AppConfig, VideoBackendPreference, load_from_path, save_validated_atomic_at,
};

/// Выполняет одну явно выбранную config-операцию и возвращает понятную ошибку.
fn run() -> Result<(), String> {
    // Забираем имя процесса отдельно, чтобы usage не зависел от Cargo invocation.
    let program_name = env::args()
        .next()
        .unwrap_or_else(|| "smoke_config".to_owned());
    // Остальные аргументы образуют маленький стабильный CLI-контракт helper-а.
    let arguments: Vec<String> = env::args().skip(1).collect();

    // Ровно две операции не позволяют helper-у превратиться в второй config editor.
    match arguments.as_slice() {
        [command, config_path, backend] if command == "generate-current" => {
            generate_current_config(PathBuf::from(config_path), backend)
        }
        [command, config_path] if command == "parse-current" => {
            parse_current_config(PathBuf::from(config_path))
        }
        _ => Err(format!(
            "usage: {program_name} generate-current <path> <auto|hardware|software>\n       \
             {program_name} parse-current <path>"
        )),
    }
}

/// Создаёт полный current-schema config с минимальными playback overrides.
fn generate_current_config(config_path: PathBuf, backend: &str) -> Result<(), String> {
    // Typed enum сохраняет тот же public vocabulary, что production config parser.
    let backend_preference = parse_backend_preference(backend)?;
    // Default является единственным владельцем полного набора актуальных полей schema v5.
    let mut config = AppConfig::default();
    // Smoke должен начать воспроизведение без взаимодействия с GUI.
    config.player.start_paused = false;
    // Каждый scenario явно задаёт проверяемую decoder policy.
    config.video.preferred_backend = backend_preference;
    // Production writer валидирует и атомарно сохраняет полный generated TOML.
    save_validated_atomic_at(&config_path, &config)
        .map_err(|error| format!("не удалось записать current config: {error}"))
}

/// Проверяет, что файл читается production loader-ом без legacy migration.
fn parse_current_config(config_path: PathBuf) -> Result<(), String> {
    // Production loader выполняет strict serde, migration dispatch и validation.
    let loaded = load_from_path(&config_path)
        .map_err(|error| format!("current config не прошёл production parse: {error}"))?;
    // Current smoke не должен незаметно войти в legacy migration path.
    if loaded.config.schema_version != rustiplayer_config::CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "ожидалась schema version {}, получена {}",
            rustiplayer_config::CURRENT_SCHEMA_VERSION,
            loaded.config.schema_version
        ));
    }
    // Успешный marker пригоден и человеку, и script self-test-у.
    println!(
        "PASS: current config parsed as schema v{}",
        loaded.config.schema_version
    );
    Ok(())
}

/// Переводит стабильный config id в typed backend preference.
fn parse_backend_preference(backend: &str) -> Result<VideoBackendPreference, String> {
    // Match перечисляет все и только поддерживаемые public значения.
    match backend {
        "auto" => Ok(VideoBackendPreference::Auto),
        "hardware" => Ok(VideoBackendPreference::Hardware),
        "software" => Ok(VideoBackendPreference::Software),
        unsupported => Err(format!(
            "неподдерживаемый backend `{unsupported}`; ожидался auto, hardware или software"
        )),
    }
}

/// Преобразует Result в process exit contract без panic и silent error.
fn main() -> ExitCode {
    // Ошибка всегда видима в stderr, а ненулевой код не считается acceptance pass.
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Ошибка: {error}");
            ExitCode::FAILURE
        }
    }
}
