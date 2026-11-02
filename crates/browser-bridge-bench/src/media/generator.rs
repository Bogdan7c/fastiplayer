//! Запуск `media/generate.sh` и кэш результата.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::error::MediaError;
use super::{CATALOG_FILE_NAME, MediaCatalog};

/// Сколько последних байт stderr скрипта показываем в ошибке (там причина, а не весь шум ffmpeg).
const STDERR_TAIL_BYTES: usize = 4000;

/// Путь к скрипту генерации внутри исходников crate-а (не хардкод места на диске: берётся
/// из `CARGO_MANIFEST_DIR` на этапе сборки).
pub fn generate_script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("media")
        .join("generate.sh")
}

/// Возвращает каталог медиа из кэша, при необходимости сгенерировав его.
///
/// Ключ кэша — sha256 содержимого скрипта: правка скрипта автоматически даёт новую папку.
/// Генерация идёт во временную папку внутри `cache_root` и затем переименовывается одним
/// атомарным `rename`; если параллельный прогон успел раньше, берём его результат.
pub fn ensure_media(cache_root: &Path) -> Result<MediaCatalog, MediaError> {
    let script = generate_script_path();
    let cache_key = script_fingerprint(&script)?;
    let final_dir = cache_root.join(&cache_key);
    if final_dir.join(CATALOG_FILE_NAME).is_file() {
        // Кэш мог быть испорчен вручную — тогда ошибка проверки скажет, что именно не так.
        return MediaCatalog::load(&final_dir);
    }
    create_dir(cache_root)?;
    let staging = tempfile::Builder::new()
        .prefix(&format!(".staging-{cache_key}-"))
        .tempdir_in(cache_root)
        .map_err(|source| io_error("create a staging directory", cache_root, source))?;
    generate_media_into(staging.path())?;
    // Проверяем ДО публикации: в кэш попадает только валидный каталог.
    MediaCatalog::load(staging.path())?;
    let staging_path = staging.keep();
    match fs::rename(&staging_path, &final_dir) {
        Ok(()) => {}
        Err(_) if final_dir.join(CATALOG_FILE_NAME).is_file() => {
            // Параллельный прогон опубликовал тот же ключ первым — наш результат лишний.
            let _ = fs::remove_dir_all(&staging_path);
        }
        Err(source) => return Err(io_error("publish the generated media", &final_dir, source)),
    }
    MediaCatalog::load(&final_dir)
}

/// Запускает скрипт, генерирующий весь набор роликов прямо в `out_dir` (без кэша).
pub fn generate_media_into(out_dir: &Path) -> Result<(), MediaError> {
    let script = generate_script_path();
    let output = Command::new("bash")
        .arg(&script)
        .arg(out_dir)
        .output()
        .map_err(|source| MediaError::GeneratorSpawn { script, source })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(MediaError::GeneratorFailed {
        status: output.status.to_string(),
        stderr_tail: tail_of(&stderr, STDERR_TAIL_BYTES),
    })
}

/// sha256 содержимого скрипта в hex — ключ папки кэша.
fn script_fingerprint(script: &Path) -> Result<String, MediaError> {
    let script_bytes =
        fs::read(script).map_err(|source| io_error("read the generator script", script, source))?;
    let digest = Sha256::digest(&script_bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn create_dir(path: &Path) -> Result<(), MediaError> {
    fs::create_dir_all(path).map_err(|source| io_error("create the cache directory", path, source))
}

fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> MediaError {
    MediaError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// Последние `max_bytes` текста по границе символа.
fn tail_of(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}
