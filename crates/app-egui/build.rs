//! Раскладка vendored FFmpeg рядом с бинарём `fastiplayer`.
//!
//! В сборке по умолчанию (`vendored-ffmpeg`) crate `ffmpeg-vendor` собирает
//! закреплённый FFmpeg в своём `OUT_DIR`, а `video-ffmpeg` пересылает сюда,
//! где лежат `.so` и тексты лицензий. Этот скрипт:
//!
//! 1. копирует `libavcodec_fastiplayer.so.N`/`libavutil_fastiplayer.so.N` и
//!    лицензии в `target/<profile>/lib/` — рядом с `target/<profile>/fastiplayer`;
//! 2. прописывает бинарю `RUNPATH=$ORIGIN/lib`, поэтому загрузчик берёт наш
//!    FFmpeg, а не системный, и папку `fastiplayer` + `lib/` можно переносить.
//!
//! В сборке `system-ffmpeg` (и без FFmpeg вовсе) метаданных нет — скрипт
//! ничего не делает, бинарь линкуется с системным FFmpeg как раньше.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

/// Метаданные `video-ffmpeg` (`links = "fastiplayer_video_ffmpeg"`).
const RUNTIME_LIB_DIR_ENV: &str = "DEP_FASTIPLAYER_VIDEO_FFMPEG_RUNTIME_LIB_DIR";
const RUNTIME_LIBRARIES_ENV: &str = "DEP_FASTIPLAYER_VIDEO_FFMPEG_RUNTIME_LIBRARIES";
const LICENSE_DIR_ENV: &str = "DEP_FASTIPLAYER_VIDEO_FFMPEG_LICENSE_DIR";

/// Каталог рядом с бинарём, куда кладутся библиотеки и лицензии.
const BUNDLED_LIBRARY_DIRECTORY_NAME: &str = "lib";

/// Разделитель списка библиотек в метаданных патча.
const RUNTIME_LIBRARIES_SEPARATOR: char = ';';

/// Сколько уровней вверх от `OUT_DIR` до `target/<profile>`:
/// `target/<profile>/build/app-egui-<hash>/out` → `target/<profile>`.
const OUT_DIR_DEPTH_BELOW_PROFILE_DIRECTORY: usize = 3;

/// Что нужно разложить рядом с бинарём.
struct VendoredFfmpegBundle {
    /// Каталог, где патч собрал разделяемые библиотеки.
    runtime_lib_dir: PathBuf,
    /// SONAME-имена библиотек (`libavcodec.so.62`, …).
    runtime_libraries: Vec<String>,
    /// Каталог с текстами лицензий FFmpeg и dav1d.
    license_dir: PathBuf,
}

fn main() {
    for variable in [RUNTIME_LIB_DIR_ENV, RUNTIME_LIBRARIES_ENV, LICENSE_DIR_ENV] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    // Нет метаданных — значит сборка с системным FFmpeg или без него.
    let bundle = match read_vendored_bundle() {
        Ok(Some(bundle)) => bundle,
        Ok(None) => return,
        Err(error) => fail(&error),
    };

    match install_bundle_next_to_binary(&bundle) {
        // Если кто-то удалит скопированную библиотеку, cargo перезапустит скрипт
        // и вернёт её: иначе бинарь молча остался бы без FFmpeg.
        Ok(installed_libraries) => {
            for installed_library in installed_libraries {
                println!("cargo:rerun-if-changed={}", installed_library.display());
            }
        }
        Err(error) => fail(&error),
    }

    // `$ORIGIN` подставляет загрузчик: это каталог самого бинаря.
    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN/{BUNDLED_LIBRARY_DIRECTORY_NAME}");
}

/// Останавливает сборку с понятным сообщением.
fn fail(error: &str) -> ! {
    eprintln!("error: cannot place vendored FFmpeg next to the fastiplayer binary: {error}");
    process::exit(1);
}

/// Читает метаданные vendored FFmpeg: `None`, если их нет, ошибка — если они неполные.
fn read_vendored_bundle() -> Result<Option<VendoredFfmpegBundle>, String> {
    let Some(runtime_lib_dir) = env::var_os(RUNTIME_LIB_DIR_ENV) else {
        return Ok(None);
    };
    let runtime_libraries = env::var(RUNTIME_LIBRARIES_ENV)
        .map_err(|_| format!("{RUNTIME_LIBRARIES_ENV} is missing"))?;
    let license_dir =
        env::var_os(LICENSE_DIR_ENV).ok_or_else(|| format!("{LICENSE_DIR_ENV} is missing"))?;

    Ok(Some(VendoredFfmpegBundle {
        runtime_lib_dir: PathBuf::from(runtime_lib_dir),
        runtime_libraries: runtime_libraries
            .split(RUNTIME_LIBRARIES_SEPARATOR)
            .filter(|library_name| !library_name.is_empty())
            .map(str::to_owned)
            .collect(),
        license_dir: PathBuf::from(license_dir),
    }))
}

/// Копирует библиотеки и лицензии в `target/<profile>/lib/`; возвращает пути библиотек.
fn install_bundle_next_to_binary(bundle: &VendoredFfmpegBundle) -> Result<Vec<PathBuf>, String> {
    if bundle.runtime_libraries.is_empty() {
        return Err(format!("{RUNTIME_LIBRARIES_ENV} lists no libraries"));
    }

    let bundled_library_directory = profile_directory()?.join(BUNDLED_LIBRARY_DIRECTORY_NAME);
    fs::create_dir_all(&bundled_library_directory)
        .map_err(|error| format!("create {}: {error}", bundled_library_directory.display()))?;

    let mut installed_libraries = Vec::with_capacity(bundle.runtime_libraries.len());
    for library_name in &bundle.runtime_libraries {
        let installed_library = bundled_library_directory.join(library_name);
        // `fs::copy` идёт по симлинку `libavcodec_fastiplayer.so.62` к настоящему файлу.
        copy_file(
            &bundle.runtime_lib_dir.join(library_name),
            &installed_library,
        )?;
        installed_libraries.push(installed_library);
    }

    let license_entries = fs::read_dir(&bundle.license_dir)
        .map_err(|error| format!("list licenses {}: {error}", bundle.license_dir.display()))?;
    for license_entry in license_entries {
        let license_entry =
            license_entry.map_err(|error| format!("read license entry: {error}"))?;
        copy_file(
            &license_entry.path(),
            &bundled_library_directory.join(license_entry.file_name()),
        )?;
    }
    Ok(installed_libraries)
}

/// Находит `target/<profile>` — каталог, где cargo кладёт бинарь `fastiplayer`.
///
/// Cargo не даёт build script-у прямого пути к выходным бинарям, но раскладка
/// `OUT_DIR` стабильна: `<profile>/build/<pkg>-<hash>/out` (в том числе с
/// `--target <triple>` и своим `CARGO_TARGET_DIR`).
fn profile_directory() -> Result<PathBuf, String> {
    let out_dir = env::var_os("OUT_DIR").ok_or("cargo did not set OUT_DIR")?;
    Path::new(&out_dir)
        .ancestors()
        .nth(OUT_DIR_DEPTH_BELOW_PROFILE_DIRECTORY)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "unexpected OUT_DIR layout: {}",
                Path::new(&out_dir).display()
            )
        })
}

/// Копирует один файл, сохраняя оба пути в тексте ошибки.
fn copy_file(from_path: &Path, to_path: &Path) -> Result<(), String> {
    fs::copy(from_path, to_path)
        .map(|_copied_bytes| ())
        .map_err(|error| {
            format!(
                "copy {} to {}: {error}",
                from_path.display(),
                to_path.display()
            )
        })
}
