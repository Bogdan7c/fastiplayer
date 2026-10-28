//! Сценарий vendored-сборки: исходники → патчи → dav1d → FFmpeg → лицензии → штамп.
//!
//! Повторная сборка пропускается, пока «отпечаток» (версии, суммы архивов и
//! патчей, аргументы configure/meson) совпадает с записанным в штампе и
//! библиотеки на месте.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::archives;
use crate::build_error::VendoredBuildError;
use crate::build_options;
use crate::commands;
use crate::soname::is_soname_file_name;
use crate::source_patches;
use crate::source_patching;
use crate::sources::{ALL_SOURCES, DAV1D_SOURCE, FFMPEG_SOURCE, PinnedSource};

/// Имя файла-штампа успешной сборки внутри install prefix FFmpeg.
const BUILD_STAMP_FILE_NAME: &str = ".fastiplayer-vendored-stamp";

/// Файл лицензии, который обязан ехать вместе с библиотеками.
struct LicenseFile {
    /// Из каких исходников берётся файл.
    source: PinnedSource,
    /// Путь внутри распакованных исходников.
    relative_path: &'static str,
    /// Имя в каталоге лицензий рядом с плеером.
    published_name: &'static str,
}

/// LGPL FFmpeg требует передавать текст лицензии; dav1d — BSD-2 с тем же требованием.
const LICENSE_FILES: [LicenseFile; 3] = [
    LicenseFile {
        source: FFMPEG_SOURCE,
        relative_path: "COPYING.LGPLv2.1",
        published_name: "FFmpeg-COPYING.LGPLv2.1",
    },
    LicenseFile {
        source: FFMPEG_SOURCE,
        relative_path: "LICENSE.md",
        published_name: "FFmpeg-LICENSE.md",
    },
    LicenseFile {
        source: DAV1D_SOURCE,
        relative_path: "COPYING",
        published_name: "dav1d-COPYING",
    },
];

/// Раскладка рабочих каталогов vendored-сборки внутри `OUT_DIR`.
#[derive(Debug, Clone)]
pub struct VendoredLayout {
    /// Кеш проверенных архивов.
    pub downloads_directory: PathBuf,
    /// Распакованные исходники.
    pub sources_directory: PathBuf,
    /// Каталоги out-of-tree сборки.
    pub build_directory: PathBuf,
    /// Install prefix статической dav1d (виден только configure FFmpeg).
    pub dav1d_prefix: PathBuf,
    /// Install prefix FFmpeg: `include/`, `lib/`, `licenses/`.
    pub ffmpeg_prefix: PathBuf,
    /// Логи каждого шага.
    pub logs_directory: PathBuf,
}

impl VendoredLayout {
    /// Строит раскладку от корневого каталога vendored-сборки.
    pub fn under(root_directory: &Path) -> Self {
        Self {
            downloads_directory: root_directory.join("downloads"),
            sources_directory: root_directory.join("sources"),
            build_directory: root_directory.join("build"),
            dav1d_prefix: root_directory.join("dav1d-prefix"),
            ffmpeg_prefix: root_directory.join("ffmpeg-prefix"),
            logs_directory: root_directory.join("logs"),
        }
    }

    /// Каталог с разделяемыми библиотеками FFmpeg.
    pub fn ffmpeg_library_directory(&self) -> PathBuf {
        self.ffmpeg_prefix.join("lib")
    }

    /// Каталог с текстами лицензий для распространения рядом с библиотеками.
    pub fn license_directory(&self) -> PathBuf {
        self.ffmpeg_prefix.join("licenses")
    }
}

/// Готовый vendored FFmpeg: где он лежит и какие файлы нужны загрузчику.
#[derive(Debug, Clone)]
pub struct VendoredFfmpegInstall {
    /// Install prefix (`include/`, `lib/`).
    pub prefix: PathBuf,
    /// Каталог разделяемых библиотек.
    pub runtime_lib_dir: PathBuf,
    /// SONAME-имена библиотек (`libavcodec.so.62`, `libavutil.so.60`).
    pub runtime_libraries: Vec<String>,
    /// Каталог с лицензиями.
    pub license_dir: PathBuf,
}

/// Собирает vendored FFmpeg или переиспользует уже собранный.
pub fn ensure_vendored_ffmpeg(
    layout: &VendoredLayout,
) -> Result<VendoredFfmpegInstall, VendoredBuildError> {
    reject_cross_compilation()?;

    let fingerprint = build_fingerprint(layout)?;
    if !is_build_current(layout, &fingerprint) {
        build_from_sources(layout)?;
        write_build_stamp(layout, &fingerprint)?;
    }

    let runtime_lib_dir = layout.ffmpeg_library_directory();
    Ok(VendoredFfmpegInstall {
        prefix: layout.ffmpeg_prefix.clone(),
        runtime_libraries: runtime_library_names(&runtime_lib_dir)?,
        runtime_lib_dir,
        license_dir: layout.license_directory(),
    })
}

/// Сборка под другую платформу не поддерживается: configure не получает toolchain цели.
fn reject_cross_compilation() -> Result<(), VendoredBuildError> {
    let host = env::var("HOST").map_err(|_| VendoredBuildError::MissingCargoEnv("HOST"))?;
    let target = env::var("TARGET").map_err(|_| VendoredBuildError::MissingCargoEnv("TARGET"))?;
    if host == target {
        Ok(())
    } else {
        Err(VendoredBuildError::CrossCompilation { host, target })
    }
}

/// Полная сборка: проверка инструментов, исходники, патчи, dav1d, FFmpeg, лицензии.
fn build_from_sources(layout: &VendoredLayout) -> Result<(), VendoredBuildError> {
    commands::ensure_tools_available(&commands::REQUIRED_BUILD_TOOLS)?;

    let dav1d_archive = archives::verified_archive(&DAV1D_SOURCE, layout)?;
    let ffmpeg_archive = archives::verified_archive(&FFMPEG_SOURCE, layout)?;
    let dav1d_sources = archives::unpack_archive(&DAV1D_SOURCE, &dav1d_archive, layout)?;
    let ffmpeg_sources = archives::unpack_archive(&FFMPEG_SOURCE, &ffmpeg_archive, layout)?;

    // Патчи — на свежераспакованное дерево (unpack_archive всегда распаковывает
    // заново, поэтому патч не может лечь дважды) и до configure.
    let crate_root = crate_root()?;
    for (source, source_directory) in [
        (&DAV1D_SOURCE, &dav1d_sources),
        (&FFMPEG_SOURCE, &ffmpeg_sources),
    ] {
        source_patching::apply_source_patches(
            source,
            source_directory,
            &crate_root,
            &layout.logs_directory,
        )?;
    }

    // Старые prefix-ы удаляем: в lib/ не должно остаться библиотек прошлой версии.
    remove_directory_if_present(&layout.dav1d_prefix)?;
    remove_directory_if_present(&layout.ffmpeg_prefix)?;
    remove_directory_if_present(&layout.build_directory)?;

    build_dav1d(layout, &dav1d_sources)?;
    build_ffmpeg(layout, &ffmpeg_sources)?;
    copy_license_files(layout, &dav1d_sources, &ffmpeg_sources)
}

/// Корень crate-а `ffmpeg-vendor`: от него лежат `patches/`.
fn crate_root() -> Result<PathBuf, VendoredBuildError> {
    env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or(VendoredBuildError::MissingCargoEnv("CARGO_MANIFEST_DIR"))
}

/// Собирает и устанавливает статическую dav1d в `dav1d_prefix`.
fn build_dav1d(layout: &VendoredLayout, dav1d_sources: &Path) -> Result<(), VendoredBuildError> {
    let dav1d_build_directory = layout.build_directory.join("dav1d");

    let mut setup_command = Command::new("meson");
    setup_command.args(build_options::dav1d_meson_setup_arguments(
        &dav1d_build_directory,
        dav1d_sources,
        &layout.dav1d_prefix,
    ));
    commands::run_logged(setup_command, "dav1d-meson-setup", &layout.logs_directory)?;

    let mut install_command = Command::new("meson");
    install_command
        .arg("install")
        .arg("-C")
        .arg(&dav1d_build_directory);
    commands::run_logged(
        install_command,
        "dav1d-meson-install",
        &layout.logs_directory,
    )
}

/// Конфигурирует, собирает и устанавливает FFmpeg в `ffmpeg_prefix`.
fn build_ffmpeg(layout: &VendoredLayout, ffmpeg_sources: &Path) -> Result<(), VendoredBuildError> {
    let ffmpeg_build_directory = layout.build_directory.join("ffmpeg");
    archives::create_directory(&ffmpeg_build_directory)?;

    let mut configure_command = Command::new(ffmpeg_sources.join("configure"));
    configure_command
        .current_dir(&ffmpeg_build_directory)
        .args(build_options::ffmpeg_configure_arguments(
            &layout.ffmpeg_prefix,
        ))
        // PKG_CONFIG_LIBDIR (а не PATH) скрывает системные .pc: configure видит
        // только нашу dav1d и не может случайно подхватить системную.
        .env(
            "PKG_CONFIG_LIBDIR",
            layout.dav1d_prefix.join("lib").join("pkgconfig"),
        )
        .env_remove("PKG_CONFIG_PATH");
    commands::run_logged(
        configure_command,
        "ffmpeg-configure",
        &layout.logs_directory,
    )?;

    let mut make_command = Command::new("make");
    make_command
        .current_dir(&ffmpeg_build_directory)
        .arg(format!("-j{}", parallel_jobs()));
    commands::run_logged(make_command, "ffmpeg-make", &layout.logs_directory)?;

    let mut install_command = Command::new("make");
    install_command
        .current_dir(&ffmpeg_build_directory)
        .arg("install");
    commands::run_logged(
        install_command,
        "ffmpeg-make-install",
        &layout.logs_directory,
    )
}

/// Число параллельных задач: cargo передаёт его через `NUM_JOBS` (уважает `-j`).
fn parallel_jobs() -> usize {
    env::var("NUM_JOBS")
        .ok()
        .and_then(|jobs_text| jobs_text.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .unwrap_or(1)
}

/// Копирует тексты лицензий FFmpeg и dav1d в `ffmpeg_prefix/licenses`.
fn copy_license_files(
    layout: &VendoredLayout,
    dav1d_sources: &Path,
    ffmpeg_sources: &Path,
) -> Result<(), VendoredBuildError> {
    let license_directory = layout.license_directory();
    archives::create_directory(&license_directory)?;

    for license_file in LICENSE_FILES {
        let source_root = if license_file.source == DAV1D_SOURCE {
            dav1d_sources
        } else {
            ffmpeg_sources
        };
        let from_path = source_root.join(license_file.relative_path);
        let to_path = license_directory.join(license_file.published_name);
        fs::copy(&from_path, &to_path).map_err(|io_error| VendoredBuildError::Io {
            action: format!(
                "copy license {} to {}",
                from_path.display(),
                to_path.display()
            ),
            source: io_error,
        })?;
    }
    Ok(())
}

/// Отпечаток всех входов сборки: смена любого из них требует пересборки.
///
/// Сюда входит SHA-256 каждого патча исходников: правка патча обязана
/// пересобрать FFmpeg, а не взять собранный без неё кэш.
fn build_fingerprint(layout: &VendoredLayout) -> Result<String, VendoredBuildError> {
    let mut fingerprint_lines: Vec<String> = ALL_SOURCES
        .iter()
        .map(|source| format!("{} {} {}", source.slug, source.version, source.sha256))
        .collect();
    fingerprint_lines.extend(
        source_patches::patch_fingerprint_lines(&crate_root()?).map_err(|checksum_error| {
            VendoredBuildError::SourcePatchUnreadable {
                details: checksum_error.to_string(),
            }
        })?,
    );
    fingerprint_lines
        .push(build_options::ffmpeg_configure_arguments(&layout.ffmpeg_prefix).join(" "));
    fingerprint_lines.push(
        build_options::dav1d_meson_setup_arguments(
            &layout.build_directory.join("dav1d"),
            Path::new(DAV1D_SOURCE.unpacked_directory_name),
            &layout.dav1d_prefix,
        )
        .join(" "),
    );
    Ok(fingerprint_lines.join("\n"))
}

/// Сборка актуальна, если штамп совпадает и библиотеки на месте.
fn is_build_current(layout: &VendoredLayout, fingerprint: &str) -> bool {
    let stamp_matches = fs::read_to_string(layout.ffmpeg_prefix.join(BUILD_STAMP_FILE_NAME))
        .is_ok_and(|stamp_text| stamp_text == fingerprint);
    stamp_matches
        && runtime_library_names(&layout.ffmpeg_library_directory())
            .is_ok_and(|library_names| !library_names.is_empty())
}

/// Записывает штамп только после полностью успешной сборки.
fn write_build_stamp(layout: &VendoredLayout, fingerprint: &str) -> Result<(), VendoredBuildError> {
    let stamp_path = layout.ffmpeg_prefix.join(BUILD_STAMP_FILE_NAME);
    fs::write(&stamp_path, fingerprint).map_err(|io_error| VendoredBuildError::Io {
        action: format!("write build stamp {}", stamp_path.display()),
        source: io_error,
    })
}

/// SONAME-имена библиотек в каталоге (`libavcodec.so.62` и т.п.), по алфавиту.
fn runtime_library_names(library_directory: &Path) -> Result<Vec<String>, VendoredBuildError> {
    let list_error = |io_error| VendoredBuildError::Io {
        action: format!("list {}", library_directory.display()),
        source: io_error,
    };
    let mut library_names = Vec::new();
    for directory_entry in fs::read_dir(library_directory).map_err(list_error)? {
        let file_name = directory_entry
            .map_err(list_error)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if is_soname_file_name(&file_name) {
            library_names.push(file_name);
        }
    }
    library_names.sort();
    Ok(library_names)
}

/// Удаляет каталог, если он существует.
fn remove_directory_if_present(directory: &Path) -> Result<(), VendoredBuildError> {
    if directory.exists() {
        fs::remove_dir_all(directory).map_err(|io_error| VendoredBuildError::Io {
            action: format!("remove {}", directory.display()),
            source: io_error,
        })?;
    }
    Ok(())
}
