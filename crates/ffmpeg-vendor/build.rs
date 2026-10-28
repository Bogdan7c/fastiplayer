//! Сборка закреплённого FFmpeg 8.1.3 + dav1d 1.5.4 вместе с плеером.
//!
//! Этот crate существует ради своего build script-а. Он:
//! 1. берёт архивы dav1d и FFmpeg (локально по env или скачивает) и
//!    обязательно сверяет их SHA-256;
//! 2. накладывает наши патчи исходников (`patches/`, строго, GNU `patch`);
//! 3. собирает dav1d статически с PIC, а FFmpeg — минимальным LGPL-набором
//!    в `libavcodec_fastiplayer.so` и `libavutil_fastiplayer.so` (уникальные
//!    имена не путаются с системным FFmpeg; dav1d оказывается внутри libavcodec);
//! 4. отдаёт прямым зависимым (`links = "fastiplayer_ffmpeg_vendor"`)
//!    метаданные: `prefix`, `link_libraries`, `runtime_lib_dir`,
//!    `runtime_libraries`, `license_dir`.
//!
//! Линкует и генерирует привязки патч `ffmpeg-sys-next` (по `prefix`), а
//! раскладывает `.so` рядом с бинарём `app-egui` (через `video-ffmpeg`).

// Чистые модули политики общие с библиотекой crate-а (там же их тесты).
#[path = "src/build_options.rs"]
mod build_options;
#[path = "src/checksum.rs"]
mod checksum;
#[path = "src/soname.rs"]
mod soname;
#[path = "src/source_patches.rs"]
mod source_patches;
#[path = "src/sources.rs"]
mod sources;

// Модули, нужные только build script-у: процессы, архивы, сценарий сборки.
#[path = "build_support/archives.rs"]
mod archives;
#[path = "build_support/build_error.rs"]
mod build_error;
#[path = "build_support/commands.rs"]
mod commands;
#[path = "build_support/source_patching.rs"]
mod source_patching;
#[path = "build_support/vendored_build.rs"]
mod vendored_build;

use std::env;
use std::path::PathBuf;
use std::process;

use build_error::VendoredBuildError;
use vendored_build::{VendoredFfmpegInstall, VendoredLayout};

/// Разделитель списков в метаданных (`a;b`).
const METADATA_LIST_SEPARATOR: &str = ";";

fn main() {
    // Любая директива `rerun-if-*` выключает «перезапуск при изменении любого
    // файла crate-а» (Cargo Book, Build Scripts → Change Detection). Поэтому
    // каталог патчей называем явно: правка патча обязана разбудить build
    // script, а нужна ли пересборка FFmpeg, решает отпечаток (там SHA-256
    // каждого патча). Сам build script и его модули cargo пересобирает сам.
    println!(
        "cargo::rerun-if-changed={}",
        source_patches::PATCHES_DIRECTORY
    );
    // Пересборка нужна также при смене пути к локальному архиву.
    for source in sources::ALL_SOURCES {
        println!("cargo::rerun-if-env-changed={}", source.local_archive_env);
    }

    match build() {
        Ok(install) => emit_metadata(&install),
        Err(build_error) => {
            eprintln!("error: {build_error}");
            process::exit(1);
        }
    }
}

/// Определяет раскладку в `OUT_DIR` и собирает (или переиспользует) FFmpeg.
fn build() -> Result<VendoredFfmpegInstall, VendoredBuildError> {
    let out_directory = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or(VendoredBuildError::MissingCargoEnv("OUT_DIR"))?;
    let layout = VendoredLayout::under(&out_directory.join("fastiplayer-vendored"));
    vendored_build::ensure_vendored_ffmpeg(&layout)
}

/// Публикует метаданные для прямых зависимых (`DEP_FASTIPLAYER_FFMPEG_VENDOR_<KEY>`).
fn emit_metadata(install: &VendoredFfmpegInstall) {
    println!("cargo::metadata=prefix={}", install.prefix.display());
    // Имена для `-l`: с суффиксом, чтобы линковщик не взял системный FFmpeg.
    println!(
        "cargo::metadata=link_libraries={}",
        build_options::link_library_names().join(METADATA_LIST_SEPARATOR)
    );
    println!(
        "cargo::metadata=runtime_lib_dir={}",
        install.runtime_lib_dir.display()
    );
    println!(
        "cargo::metadata=runtime_libraries={}",
        install.runtime_libraries.join(METADATA_LIST_SEPARATOR)
    );
    println!(
        "cargo::metadata=license_dir={}",
        install.license_dir.display()
    );
}
