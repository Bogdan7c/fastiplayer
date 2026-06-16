//! Build-policy guard для optional FFmpeg feature.
//!
//! Этот build script намеренно ничего не проверяет в default build-е. FFmpeg
//! должен быть runtime capability, а не условие сборки всего workspace.

use std::env;
use std::path::{Path, PathBuf};
use std::process;

/// Cargo feature, который включает raw FFmpeg FFI dependency.
const FFMPEG_FEATURE_ENV: &str = "CARGO_FEATURE_FFMPEG";

/// Env var, которую понимает `ffmpeg-sys-next` для явного FFmpeg prefix-а.
const FFMPEG_SYS_PREFIX_ENV: &str = "FFMPEG_DIR";

/// Проектный env var из tooling; сам `ffmpeg-sys-next` его не читает.
const RUSTIPLAYER_PREFIX_ENV: &str = "RUSTIPLAYER_FFMPEG_PREFIX";

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

    if let Err(error) = verify_ffmpeg_build_inputs() {
        eprintln!("{error}");
        process::exit(1);
    }
}

/// Фиксирует env inputs, которые должны пересобирать crate при изменении.
fn emit_rerun_inputs() {
    println!("cargo:rerun-if-env-changed={FFMPEG_FEATURE_ENV}");
    println!("cargo:rerun-if-env-changed={FFMPEG_SYS_PREFIX_ENV}");
    println!("cargo:rerun-if-env-changed={RUSTIPLAYER_PREFIX_ENV}");
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

    if let Some(project_prefix) = env::var_os(RUSTIPLAYER_PREFIX_ENV) {
        let project_prefix = PathBuf::from(project_prefix);
        return Err(format!(
            "FFmpeg feature включён, но задан только {RUSTIPLAYER_PREFIX_ENV}={}. \
             `ffmpeg-sys-next` читает {FFMPEG_SYS_PREFIX_ENV}, поэтому выставь \
             {FFMPEG_SYS_PREFIX_ENV}={} или добавь prefix pkg-config directory в PKG_CONFIG_PATH.",
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
            "FFmpeg feature включён, но {FFMPEG_SYS_PREFIX_ENV}={} не существует.",
            prefix.display()
        ));
    }

    let include_dir = prefix.join("include");
    for required_header in REQUIRED_HEADERS {
        let header_path = include_dir.join(required_header);
        if !header_path.is_file() {
            return Err(format!(
                "FFmpeg feature включён, но header `{}` не найден. \
                 Ожидался путь: {}",
                required_header,
                header_path.display()
            ));
        }
    }

    let library_dir = find_dynamic_library_dir(prefix).ok_or_else(|| {
        format!(
            "FFmpeg feature включён, но dynamic lib directory не найден под {}. \
             Ожидался один из каталогов: lib, lib64 или lib/arm64.",
            prefix.display()
        )
    })?;

    for required_library in REQUIRED_DYNAMIC_LIBS {
        if !contains_dynamic_library(&library_dir, required_library) {
            return Err(format!(
                "FFmpeg feature включён, но dynamic library `{}` не найдена в {}.",
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
        "FFmpeg feature включён, но pkg-config не нашёл `{library_name}`: {error}\n\
         Установи FFmpeg development package или собери локальный dynamic LGPL FFmpeg через \
         scripts/tooling/build-ffmpeg-lgpl.sh, затем выставь один из вариантов:\n\
         - {FFMPEG_SYS_PREFIX_ENV}=/path/to/ffmpeg-prefix\n\
         - PKG_CONFIG_PATH=/path/to/ffmpeg-prefix/lib/pkgconfig:$PKG_CONFIG_PATH\n\
         Default workspace build не требует FFmpeg: не включай feature `ffmpeg`, если \
         headers/libs недоступны."
    )
}
