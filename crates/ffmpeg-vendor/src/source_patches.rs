//! Патчи исходников закреплённого FFmpeg (решение владельца 83, сессия 13
//! плана `user/gles2-renderer/`).
//!
//! Патч — обычный unified diff в `patches/<каталог исходников>/`. Он
//! накладывается GNU `patch` сразу после распаковки архива и до configure:
//! - **строго**: без fuzz (контекст обязан совпасть точно), уже наложенный или
//!   развёрнутый патч — ошибка, а не пропуск;
//! - **атомарно**: сначала пробный прогон (`--dry-run`), и только если он
//!   прошёл — настоящий. Не легший патч не оставляет полу-изменённое дерево;
//! - **с пересборкой**: SHA-256 файла патча входит в отпечаток сборки, поэтому
//!   правка патча пересобирает FFmpeg, а не берёт старый кэш.
//!
//! Модуль чистый (без cargo-директив): его подключает build script, а здесь
//! его закрепляют unit-тесты. Запуск процесса `patch` и ошибки сборки живут в
//! `build_support/source_patching.rs`.
//!
//! Миграция на новый FFmpeg: перенести файл в каталог новой версии, обновить
//! [`SourcePatch::target`] и пройти чек-лист записи `[[source_patch]]` в
//! `docs/dependency-patches.toml`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::checksum::{ChecksumError, sha256_of_file};
use crate::sources::{FFMPEG_SOURCE, PinnedSource};

/// Каталог патчей относительно корня crate-а `ffmpeg-vendor`.
pub const PATCHES_DIRECTORY: &str = "patches";

/// Сколько компонентов пути срезать (`a/libavcodec/…` → `libavcodec/…`):
/// патчи пишутся как `diff -u a/… b/…` / `git format-patch`.
const PATCH_STRIP_COMPONENTS: &str = "-p1";

/// Один патч исходников закреплённой библиотеки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePatch {
    /// Короткое имя для логов и сообщений (`0001-libdav1d-direct-rendering`).
    pub name: &'static str,

    /// К каким исходникам патч относится (версия входит в путь к файлу).
    pub target: PinnedSource,

    /// Имя файла внутри `patches/<target.unpacked_directory_name>/`.
    pub file_name: &'static str,
}

impl SourcePatch {
    /// Путь к файлу патча относительно корня crate-а.
    pub fn relative_path(&self) -> PathBuf {
        Path::new(PATCHES_DIRECTORY)
            .join(self.target.unpacked_directory_name)
            .join(self.file_name)
    }

    /// Строка отпечатка сборки: имя, версия цели и сумма содержимого патча.
    pub fn fingerprint_line(&self, patch_sha256: &str) -> String {
        format!(
            "patch {} {} {} {patch_sha256}",
            self.name, self.target.slug, self.target.version
        )
    }
}

/// libdav1d с `AV_CODEC_CAP_DR1`: dav1d пишет кадры AV1 в память, которую
/// выдаёт `get_buffer2()` плеера (импортируемые блоки «без копии»).
pub const FFMPEG_LIBDAV1D_DIRECT_RENDERING: SourcePatch = SourcePatch {
    name: "0001-libdav1d-direct-rendering",
    target: FFMPEG_SOURCE,
    file_name: "0001-libdav1d-direct-rendering.patch",
};

/// Все патчи в порядке наложения.
pub const ALL_SOURCE_PATCHES: [SourcePatch; 1] = [FFMPEG_LIBDAV1D_DIRECT_RENDERING];

/// Патчи для конкретных исходников в порядке наложения.
pub fn patches_for(source: &PinnedSource) -> impl Iterator<Item = &'static SourcePatch> + '_ {
    ALL_SOURCE_PATCHES
        .iter()
        .filter(move |patch| patch.target == *source)
}

/// Проход GNU `patch`: пробный (ничего не меняет) или настоящий.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchPass {
    /// Проверить, что каждый hunk ложится точно, не трогая файлы.
    DryRun,
    /// Наложить патч (только после успешного [`PatchPass::DryRun`]).
    Apply,
}

/// Аргументы GNU `patch` для строгого наложения в каталог исходников.
pub fn gnu_patch_arguments(
    source_directory: &Path,
    patch_file: &Path,
    pass: PatchPass,
) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = vec![
        PATCH_STRIP_COMPONENTS.into(),
        // Контекст hunk-а обязан совпасть целиком: «подгонка» могла бы молча
        // вклеить правку не туда в новой версии FFmpeg.
        "--fuzz=0".into(),
        // Уже наложенный (развёрнутый) патч — отказ, а не вопрос «откатить?».
        "--forward".into(),
        // Никаких интерактивных вопросов в build script-е.
        "--batch".into(),
        // Не оставлять `.orig` рядом с исходниками.
        "--no-backup-if-mismatch".into(),
        "--directory".into(),
        source_directory.as_os_str().to_owned(),
        "--input".into(),
        patch_file.as_os_str().to_owned(),
    ];
    if pass == PatchPass::DryRun {
        arguments.push("--dry-run".into());
    }
    arguments
}

/// Строки отпечатка для всех патчей: смена содержимого любого патча меняет
/// отпечаток и запускает пересборку FFmpeg.
pub fn patch_fingerprint_lines(crate_root: &Path) -> Result<Vec<String>, ChecksumError> {
    ALL_SOURCE_PATCHES
        .iter()
        .map(|patch| {
            let patch_sha256 = sha256_of_file(&crate_root.join(patch.relative_path()))?;
            Ok(patch.fingerprint_line(&patch_sha256))
        })
        .collect()
}

// Путь явный: модуль подключается и библиотекой, и build script-ом через `#[path]`,
// а у таких модулей вложенные файлы ищутся не там, где у обычных.
#[cfg(test)]
#[path = "source_patches/tests.rs"]
mod tests;
