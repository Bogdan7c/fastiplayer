//! Build-policy guard для optional FFmpeg feature.
//!
//! Этот build script намеренно ничего не проверяет в default build-е. FFmpeg
//! должен быть runtime capability, а не условие сборки всего workspace.
//!
//! Два режима FFmpeg:
//! - `vendored-ffmpeg`: закреплённый FFmpeg собирает crate `ffmpeg-vendor`;
//!   здесь только пересылаем его метаданные (где лежат `.so` и лицензии)
//!   дальше в `app-egui`, который кладёт библиотеки рядом с бинарём;
//! - `ffmpeg` без vendored: системный FFmpeg, проверяем pkg-config/`FFMPEG_DIR`.

use std::env;
use std::path::{Path, PathBuf};
use std::process;

/// Cargo feature, который включает raw FFmpeg FFI dependency.
const FFMPEG_FEATURE_ENV: &str = "CARGO_FEATURE_FFMPEG";

/// Cargo feature закреплённой (vendored) сборки FFmpeg.
const VENDORED_FFMPEG_FEATURE_ENV: &str = "CARGO_FEATURE_VENDORED_FFMPEG";

/// Метаданные `ffmpeg-vendor` (`links = "fastiplayer_ffmpeg_vendor"`), которые
/// пересылаем дальше. Формат: (переменная от ffmpeg-vendor, ключ для `app-egui`).
const FORWARDED_VENDORED_METADATA: [(&str, &str); 3] = [
    (
        "DEP_FASTIPLAYER_FFMPEG_VENDOR_RUNTIME_LIB_DIR",
        "runtime_lib_dir",
    ),
    (
        "DEP_FASTIPLAYER_FFMPEG_VENDOR_RUNTIME_LIBRARIES",
        "runtime_libraries",
    ),
    ("DEP_FASTIPLAYER_FFMPEG_VENDOR_LICENSE_DIR", "license_dir"),
];

/// Env var, которую понимает `ffmpeg-sys-next` для явного FFmpeg prefix-а.
const FFMPEG_SYS_PREFIX_ENV: &str = "FFMPEG_DIR";

/// Проектный env var из tooling; сам `ffmpeg-sys-next` его не читает.
const FASTIPLAYER_PREFIX_ENV: &str = "FASTIPLAYER_FFMPEG_PREFIX";

/// Минимальный набор dynamic libav*, нужный software video decoder scaffold-у.
const REQUIRED_PKG_CONFIG_LIBS: &[&str] = &["libavutil", "libavcodec"];

/// Заголовки, без которых raw bindings не смогут сгенерироваться.
const REQUIRED_HEADERS: &[&str] = &["libavutil/avutil.h", "libavcodec/avcodec.h"];

/// Dynamic libraries, которые должны быть доступны linker-у.
const REQUIRED_DYNAMIC_LIBS: &[&str] = &["avutil", "avcodec"];

fn main() {
    emit_rerun_inputs();

    if env::var_os(FFMPEG_FEATURE_ENV).is_none() {
        return;
    }

    // Vendored-режим: системный FFmpeg не нужен, проверять pkg-config незачем.
    let build_result = if env::var_os(VENDORED_FFMPEG_FEATURE_ENV).is_some() {
        forward_vendored_metadata()
    } else {
        verify_ffmpeg_build_inputs()
    };

    if let Err(error) = build_result {
        eprintln!("{error}");
        process::exit(1);
    }
}

/// Пересылает в `app-egui` (через `links`) пути к vendored `.so` и лицензиям.
///
/// Cargo отдаёт метаданные только прямым зависимым, а `app-egui` зависит от
/// `video-ffmpeg`, но не от `ffmpeg-vendor` — поэтому нужен этот шаг.
fn forward_vendored_metadata() -> Result<(), String> {
    for (vendor_variable, forwarded_key) in FORWARDED_VENDORED_METADATA {
        println!("cargo:rerun-if-env-changed={vendor_variable}");
        let metadata_value = env::var(vendor_variable).map_err(|_| {
            format!(
                "feature `vendored-ffmpeg` is enabled, but ffmpeg-vendor did not report {vendor_variable}"
            )
        })?;
        println!("cargo::metadata={forwarded_key}={metadata_value}");
    }
    Ok(())
}

/// Фиксирует env inputs, которые должны пересобирать crate при изменении.
fn emit_rerun_inputs() {
    println!("cargo:rerun-if-env-changed={FFMPEG_FEATURE_ENV}");
    println!("cargo:rerun-if-env-changed={VENDORED_FFMPEG_FEATURE_ENV}");
    println!("cargo:rerun-if-env-changed={FFMPEG_SYS_PREFIX_ENV}");
    println!("cargo:rerun-if-env-changed={FASTIPLAYER_PREFIX_ENV}");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_LIBDIR");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_SYSROOT_DIR");
}

/// Проверяет только те inputs, которые нужны при явно включённом FFmpeg feature.
fn verify_ffmpeg_build_inputs() -> Result<(), String> {
    match explicit_ffmpeg_prefix()? {
        Some(prefix) => verify_explicit_prefix(&prefix),
        None => verify_pkg_config_inputs(),
    }
}

/// Возвращает явный prefix для `ffmpeg-sys-next` или понятную ошибку alias-а.
fn explicit_ffmpeg_prefix() -> Result<Option<PathBuf>, String> {
    if let Some(prefix) = env::var_os(FFMPEG_SYS_PREFIX_ENV) {
        return Ok(Some(PathBuf::from(prefix)));
    }

    if let Some(project_prefix) = env::var_os(FASTIPLAYER_PREFIX_ENV) {
        let project_prefix = PathBuf::from(project_prefix);
        return Err(format!(
            "FFmpeg feature is enabled, but only {FASTIPLAYER_PREFIX_ENV}={} is set. `ffmpeg-sys-next` reads {FFMPEG_SYS_PREFIX_ENV}; set {FFMPEG_SYS_PREFIX_ENV}={} or add the prefix pkg-config directory to PKG_CONFIG_PATH.",
            project_prefix.display(),
            project_prefix.display()
        ));
    }

    Ok(None)
}

/// Проверяет headers/libs в explicit prefix-е, не подменяя build.rs dependency.
fn verify_explicit_prefix(prefix: &Path) -> Result<(), String> {
    if !prefix.exists() {
        return Err(format!(
            "FFmpeg feature is included, but {FFMPEG_SYS_PREFIX_ENV}={} does not exist.",
            prefix.display()
        ));
    }

    let include_dir = prefix.join("include");
    for required_header in REQUIRED_HEADERS {
        let header_path = include_dir.join(required_header);
        if !header_path.is_file() {
            return Err(format!(
                "FFmpeg feature on, but header '{}' not found. Track expected: {}",
                required_header,
                header_path.display()
            ));
        }
    }

    let library_dir = find_dynamic_library_dir(prefix).ok_or_else(|| {
        format!(
            "FFmpeg feature is enabled, but no dynamic library directory was found under {}. Expected one of: lib, lib64, or lib/arm64.",
            prefix.display()
        )
    })?;

    for required_library in REQUIRED_DYNAMIC_LIBS {
        if !contains_dynamic_library(&library_dir, required_library) {
            return Err(format!(
                "FFmpeg feature is included, but dynamic library '{}' is not found in {}.",
                required_library,
                library_dir.display()
            ));
        }
    }

    Ok(())
}

/// Ищет стандартный каталог dynamic libraries внутри FFmpeg prefix-а.
fn find_dynamic_library_dir(prefix: &Path) -> Option<PathBuf> {
    ["lib", "lib64", "lib/arm64"]
        .into_iter()
        .map(|relative_path| prefix.join(relative_path))
        .find(|candidate| candidate.is_dir())
}

/// Проверяет наличие versioned или unversioned dynamic libav file.
fn contains_dynamic_library(library_dir: &Path, library_name: &str) -> bool {
    let Ok(directory_entries) = std::fs::read_dir(library_dir) else {
        return false;
    };

    let unix_prefix = format!("lib{library_name}.so");
    let macos_prefix = format!("lib{library_name}.");
    let windows_prefix = format!("{library_name}.");

    directory_entries.filter_map(Result::ok).any(|entry| {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        file_name.starts_with(&unix_prefix)
            || (file_name.starts_with(&macos_prefix) && file_name.ends_with(".dylib"))
            || (file_name.starts_with(&windows_prefix) && file_name.ends_with(".dll"))
    })
}

/// Проверяет system/pkg-config путь без эмита duplicate linker metadata.
fn verify_pkg_config_inputs() -> Result<(), String> {
    for library_name in REQUIRED_PKG_CONFIG_LIBS {
        pkg_config::Config::new()
            .cargo_metadata(false)
            .statik(false)
            .probe(library_name)
            .map_err(|error| format_pkg_config_error(library_name, error))?;
    }

    Ok(())
}

/// Форматирует ошибку так, чтобы пользователь понимал, как включить feature.
fn format_pkg_config_error(library_name: &str, error: pkg_config::Error) -> String {
    format!(
        "FFmpeg feature is enabled, but pkg-config could not find `{library_name}`: {error}\nThis is the `system-ffmpeg` build. Install the FFmpeg development package (FFmpeg 8.1+), or drop `system-ffmpeg` to use the default vendored FFmpeg, or point to your own prefix with one of:\n- {FFMPEG_SYS_PREFIX_ENV}=/path/to/ffmpeg-prefix\n- PKG_CONFIG_PATH=/path/to/ffmpeg-prefix/lib/pkgconfig:$PKG_CONFIG_PATH\nThe default workspace build does not require FFmpeg: leave feature `ffmpeg` disabled when headers/libraries are unavailable."
    )
}
