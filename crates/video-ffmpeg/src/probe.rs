use crate::FFMPEG_SOFTWARE_BACKEND_ID;

/// Минимальная версия libavcodec для baseline FFmpeg 8.1.x.
pub const MINIMUM_LIBAVCODEC_VERSION: FfmpegLibraryVersion = FfmpegLibraryVersion::new(62, 28, 0);

/// Минимальная версия libavutil для baseline FFmpeg 8.1.x.
pub const MINIMUM_LIBAVUTIL_VERSION: FfmpegLibraryVersion = FfmpegLibraryVersion::new(60, 26, 0);

/// Build status, который не требует runtime FFmpeg calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegBuildStatus {
    /// Crate собран без optional FFmpeg raw binding dependency.
    FeatureDisabled,

    /// Crate собран с optional raw binding dependency.
    FeatureEnabled,
}

/// Runtime-библиотека FFmpeg, которая нужна software backend-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegRuntimeLibrary {
    /// libavcodec содержит codec registry и будущий software decoder API.
    LibAvCodec,

    /// libavutil содержит shared version/pixel-format/frame utility API.
    LibAvUtil,
}

impl FfmpegRuntimeLibrary {
    /// Возвращает короткое имя для diagnostics без привязки к platform suffix.
    #[must_use]
    pub const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::LibAvCodec => "libavcodec",
            Self::LibAvUtil => "libavutil",
        }
    }
}

/// Версия одной FFmpeg/libav runtime-библиотеки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FfmpegLibraryVersion {
    major: u32,
    minor: u32,
    micro: u32,
}

impl FfmpegLibraryVersion {
    /// Создаёт версию из обычных major/minor/micro компонентов.
    #[must_use]
    pub const fn new(major: u32, minor: u32, micro: u32) -> Self {
        Self {
            major,
            minor,
            micro,
        }
    }

    /// Достаёт major/minor/micro из packed integer FFmpeg `AV_VERSION_INT`.
    #[must_use]
    pub const fn from_packed(packed_version: u32) -> Self {
        Self {
            major: (packed_version >> 16) & 0xff,
            minor: (packed_version >> 8) & 0xff,
            micro: packed_version & 0xff,
        }
    }

    /// Возвращает major component для typed diagnostics.
    #[must_use]
    pub const fn major(self) -> u32 {
        self.major
    }

    /// Возвращает minor component для typed diagnostics.
    #[must_use]
    pub const fn minor(self) -> u32 {
        self.minor
    }

    /// Возвращает micro component для typed diagnostics.
    #[must_use]
    pub const fn micro(self) -> u32 {
        self.micro
    }

    /// Возвращает compact dotted version для человекочитаемых diagnostics.
    #[must_use]
    pub fn display(self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.micro)
    }

    /// Проверяет одну библиотечную версию по lexicographic major/minor/micro order.
    #[must_use]
    pub const fn meets_minimum(self, minimum: Self) -> bool {
        self.major > minimum.major
            || (self.major == minimum.major
                && (self.minor > minimum.minor
                    || (self.minor == minimum.minor && self.micro >= minimum.micro)))
    }
}

/// Runtime-версии FFmpeg библиотек, которые probe проверяет вместе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegLibraryVersions {
    avcodec: FfmpegLibraryVersion,
    avutil: FfmpegLibraryVersion,
}

impl FfmpegLibraryVersions {
    /// Создаёт согласованный снимок версий libavcodec/libavutil.
    #[must_use]
    pub const fn new(
        avcodec: FfmpegLibraryVersion,
        avutil: FfmpegLibraryVersion,
    ) -> FfmpegLibraryVersions {
        FfmpegLibraryVersions { avcodec, avutil }
    }

    /// Возвращает runtime version libavcodec.
    #[must_use]
    pub const fn avcodec(self) -> FfmpegLibraryVersion {
        self.avcodec
    }

    /// Возвращает runtime version libavutil.
    #[must_use]
    pub const fn avutil(self) -> FfmpegLibraryVersion {
        self.avutil
    }

    /// Проверяет, что обе runtime-библиотеки не старее baseline.
    #[must_use]
    pub const fn meets_minimum(self, minimum: FfmpegLibraryVersions) -> bool {
        self.avcodec.meets_minimum(minimum.avcodec) && self.avutil.meets_minimum(minimum.avutil)
    }
}

/// Минимальные runtime-версии FFmpeg/libav для software backend scaffold-а.
#[must_use]
pub const fn minimum_supported_versions() -> FfmpegLibraryVersions {
    FfmpegLibraryVersions::new(MINIMUM_LIBAVCODEC_VERSION, MINIMUM_LIBAVUTIL_VERSION)
}

/// Успешный результат runtime probe без открытия player pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegRuntimeInfo {
    versions: FfmpegLibraryVersions,
}

impl FfmpegRuntimeInfo {
    /// Создаёт runtime info после успешных version/API smoke checks.
    #[must_use]
    pub const fn new(versions: FfmpegLibraryVersions) -> Self {
        Self { versions }
    }

    /// Возвращает проверенные версии libavcodec/libavutil.
    #[must_use]
    pub const fn versions(self) -> FfmpegLibraryVersions {
        self.versions
    }
}

/// Диагностируемая причина недоступности FFmpeg software capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfmpegProbeFailure {
    /// Crate собран без FFmpeg feature, поэтому runtime probe намеренно не выполнялся.
    NoBuild,

    /// Runtime dynamic loader не нашёл одну из обязательных libav* библиотек.
    MissingRuntimeLibraries {
        /// Какая library boundary не загрузилась.
        library: FfmpegRuntimeLibrary,

        /// Подробность от dynamic loader-а и список имён, которые пробовались.
        details: String,
    },

    /// Runtime FFmpeg найден, но его version baseline слишком старый.
    TooOld {
        /// Минимально поддерживаемые версии.
        minimum: FfmpegLibraryVersions,

        /// Фактически найденные runtime версии.
        found: FfmpegLibraryVersions,
    },

    /// Runtime найден, но базовый version/codec/pixel-format API не прошёл smoke check.
    ProbeFailed {
        /// Узкий шаг probe, чтобы diagnostics не сливались в общий bool.
        step: &'static str,

        /// Подробность ошибки без raw FFmpeg pointer-ов наружу.
        details: String,
    },
}

impl FfmpegProbeFailure {
    /// Stable diagnostic code для capability UI/logs.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::NoBuild => "no-build",
            Self::MissingRuntimeLibraries { .. } => "missing-runtime-libs",
            Self::TooOld { .. } => "too-old",
            Self::ProbeFailed { .. } => "probe-failed",
        }
    }
}

/// Runtime часть probe report-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfmpegRuntimeProbeStatus {
    /// Только compile-time часть была запрошена, runtime calls не выполнялись.
    NotRun,

    /// Runtime FFmpeg найден и прошёл version/API smoke checks.
    Available(FfmpegRuntimeInfo),

    /// Runtime FFmpeg недоступен с typed diagnostic cause.
    Unavailable(FfmpegProbeFailure),
}

/// Probe report для будущего capability scanner-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegProbeReport {
    /// Canonical backend id для diagnostic/capability layers.
    backend_id: &'static str,

    /// Compile-time status optional FFmpeg feature-а.
    build_status: FfmpegBuildStatus,

    /// Runtime FFmpeg availability, если runtime probe был запущен.
    runtime_status: FfmpegRuntimeProbeStatus,
}

impl FfmpegProbeReport {
    /// Возвращает backend id без создания dependency на capability scanner.
    #[must_use]
    pub const fn backend_id(&self) -> &'static str {
        self.backend_id
    }

    /// Возвращает compile-time status FFmpeg support-а.
    #[must_use]
    pub const fn build_status(&self) -> FfmpegBuildStatus {
        self.build_status
    }

    /// Возвращает runtime status без открытия decoder/pipeline.
    #[must_use]
    pub const fn runtime_status(&self) -> &FfmpegRuntimeProbeStatus {
        &self.runtime_status
    }

    /// Возвращает true только когда build и runtime FFmpeg готовы к future capability use.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self.runtime_status, FfmpegRuntimeProbeStatus::Available(_))
    }

    /// Возвращает failure, если runtime probe уже выполнился и FFmpeg недоступен.
    #[must_use]
    pub const fn failure(&self) -> Option<&FfmpegProbeFailure> {
        match &self.runtime_status {
            FfmpegRuntimeProbeStatus::Unavailable(failure) => Some(failure),
            FfmpegRuntimeProbeStatus::NotRun | FfmpegRuntimeProbeStatus::Available(_) => None,
        }
    }
}

/// Возвращает только compile-time status; runtime calls здесь намеренно не выполняются.
#[must_use]
pub const fn compile_time_probe() -> FfmpegProbeReport {
    FfmpegProbeReport {
        backend_id: FFMPEG_SOFTWARE_BACKEND_ID,
        build_status: current_build_status(),
        runtime_status: FfmpegRuntimeProbeStatus::NotRun,
    }
}

/// Проверяет runtime availability FFmpeg как capability, не открывая player pipeline.
#[must_use]
pub fn probe_runtime_availability() -> FfmpegProbeReport {
    let build_status = current_build_status();

    #[cfg(not(feature = "ffmpeg"))]
    {
        report_from_runtime_status(
            build_status,
            FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::NoBuild),
        )
    }

    #[cfg(feature = "ffmpeg")]
    {
        report_from_runtime_status(build_status, execute_runtime_probe())
    }
}

/// Изолирует cfg expression в одном месте для читаемых tests/diagnostics.
const fn current_build_status() -> FfmpegBuildStatus {
    if cfg!(feature = "ffmpeg") {
        FfmpegBuildStatus::FeatureEnabled
    } else {
        FfmpegBuildStatus::FeatureDisabled
    }
}

/// Собирает public report из уже классифицированного runtime status.
pub(crate) fn report_from_runtime_status(
    build_status: FfmpegBuildStatus,
    runtime_status: FfmpegRuntimeProbeStatus,
) -> FfmpegProbeReport {
    FfmpegProbeReport {
        backend_id: FFMPEG_SOFTWARE_BACKEND_ID,
        build_status,
        runtime_status,
    }
}

/// Классифицирует успешные raw версии в public runtime status.
#[cfg(any(feature = "ffmpeg", test))]
fn classify_runtime_versions(versions: FfmpegLibraryVersions) -> FfmpegRuntimeProbeStatus {
    let minimum = minimum_supported_versions();

    if versions.meets_minimum(minimum) {
        FfmpegRuntimeProbeStatus::Available(FfmpegRuntimeInfo::new(versions))
    } else {
        FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::TooOld {
            minimum,
            found: versions,
        })
    }
}

#[cfg(feature = "ffmpeg")]
fn execute_runtime_probe() -> FfmpegRuntimeProbeStatus {
    match runtime_loader::probe_runtime() {
        Ok(versions) => classify_runtime_versions(versions),
        Err(error) => FfmpegRuntimeProbeStatus::Unavailable(error),
    }
}

#[cfg(feature = "ffmpeg")]
mod runtime_loader {
    use super::{
        FfmpegLibraryVersion, FfmpegLibraryVersions, FfmpegProbeFailure, FfmpegRuntimeLibrary,
    };
    use libloading::Library;
    use std::os::raw::{c_uint, c_void};

    /// ABI type для `avcodec_version`/`avutil_version`.
    type VersionFunction = unsafe extern "C" fn() -> c_uint;

    /// ABI type для `avcodec_find_decoder`.
    type FindDecoderFunction = unsafe extern "C" fn(c_uint) -> *const c_void;

    /// ABI type для `av_pix_fmt_desc_get`.
    type PixelFormatDescriptorFunction = unsafe extern "C" fn(c_uint) -> *const c_void;

    /// Стабильное FFmpeg enum value для `AV_CODEC_ID_H264` из C headers.
    const AV_CODEC_ID_H264: c_uint = 27;

    /// Стабильное FFmpeg enum value для `AV_PIX_FMT_YUV420P` из C headers.
    const AV_PIX_FMT_YUV420P: c_uint = 0;

    /// libavcodec SONAME candidates от future/current к older fallback names.
    #[cfg(target_os = "linux")]
    const AVCODEC_LIBRARY_CANDIDATES: &[&str] = &[
        "libavcodec.so.64",
        "libavcodec.so.63",
        "libavcodec.so.62",
        "libavcodec.so",
        "libavcodec.so.61",
        "libavcodec.so.60",
        "libavcodec.so.59",
        "libavcodec.so.58",
    ];

    /// libavutil SONAME candidates от future/current к older fallback names.
    #[cfg(target_os = "linux")]
    const AVUTIL_LIBRARY_CANDIDATES: &[&str] = &[
        "libavutil.so.62",
        "libavutil.so.61",
        "libavutil.so.60",
        "libavutil.so",
        "libavutil.so.59",
        "libavutil.so.58",
        "libavutil.so.57",
        "libavutil.so.56",
    ];

    /// macOS dylib candidates для libavcodec.
    #[cfg(target_os = "macos")]
    const AVCODEC_LIBRARY_CANDIDATES: &[&str] = &[
        "libavcodec.64.dylib",
        "libavcodec.63.dylib",
        "libavcodec.62.dylib",
        "libavcodec.dylib",
        "libavcodec.61.dylib",
        "libavcodec.60.dylib",
        "libavcodec.59.dylib",
        "libavcodec.58.dylib",
    ];

    /// macOS dylib candidates для libavutil.
    #[cfg(target_os = "macos")]
    const AVUTIL_LIBRARY_CANDIDATES: &[&str] = &[
        "libavutil.62.dylib",
        "libavutil.61.dylib",
        "libavutil.60.dylib",
        "libavutil.dylib",
        "libavutil.59.dylib",
        "libavutil.58.dylib",
        "libavutil.57.dylib",
        "libavutil.56.dylib",
    ];

    /// Windows DLL candidates для libavcodec.
    #[cfg(target_os = "windows")]
    const AVCODEC_LIBRARY_CANDIDATES: &[&str] = &[
        "avcodec-64.dll",
        "avcodec-63.dll",
        "avcodec-62.dll",
        "avcodec.dll",
        "avcodec-61.dll",
        "avcodec-60.dll",
        "avcodec-59.dll",
        "avcodec-58.dll",
    ];

    /// Windows DLL candidates для libavutil.
    #[cfg(target_os = "windows")]
    const AVUTIL_LIBRARY_CANDIDATES: &[&str] = &[
        "avutil-62.dll",
        "avutil-61.dll",
        "avutil-60.dll",
        "avutil.dll",
        "avutil-59.dll",
        "avutil-58.dll",
        "avutil-57.dll",
        "avutil-56.dll",
    ];

    /// Осторожные fallback names для unsupported target OSes.
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    const AVCODEC_LIBRARY_CANDIDATES: &[&str] = &["libavcodec"];

    /// Осторожные fallback names для unsupported target OSes.
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    const AVUTIL_LIBRARY_CANDIDATES: &[&str] = &["libavutil"];

    /// Загружает libav* runtime и выполняет минимальный version/API smoke check.
    pub(super) fn probe_runtime() -> Result<FfmpegLibraryVersions, FfmpegProbeFailure> {
        let avutil = load_library(FfmpegRuntimeLibrary::LibAvUtil, AVUTIL_LIBRARY_CANDIDATES)?;
        let avcodec = load_library(FfmpegRuntimeLibrary::LibAvCodec, AVCODEC_LIBRARY_CANDIDATES)?;

        let avutil_version = call_version_function(
            &avutil,
            FfmpegRuntimeLibrary::LibAvUtil,
            b"avutil_version\0",
            "avutil_version",
        )?;
        let avcodec_version = call_version_function(
            &avcodec,
            FfmpegRuntimeLibrary::LibAvCodec,
            b"avcodec_version\0",
            "avcodec_version",
        )?;

        require_decoder_api(&avcodec)?;
        require_pixel_format_api(&avutil)?;

        Ok(FfmpegLibraryVersions::new(
            FfmpegLibraryVersion::from_packed(avcodec_version),
            FfmpegLibraryVersion::from_packed(avutil_version),
        ))
    }

    /// Пытается загрузить одну runtime-библиотеку по platform-specific candidate names.
    fn load_library(
        library: FfmpegRuntimeLibrary,
        candidates: &'static [&'static str],
    ) -> Result<Library, FfmpegProbeFailure> {
        let mut attempt_details = Vec::new();

        for candidate in candidates {
            // SAFETY: `Library::new` запускает platform loader; мы не берём symbols здесь.
            match unsafe { Library::new(candidate) } {
                Ok(loaded_library) => return Ok(loaded_library),
                Err(error) => attempt_details.push(format!("{candidate}: {error}")),
            }
        }

        Err(FfmpegProbeFailure::MissingRuntimeLibraries {
            library,
            details: attempt_details.join("; "),
        })
    }

    /// Достаёт и вызывает version function из уже загруженной runtime library.
    fn call_version_function(
        library: &Library,
        runtime_library: FfmpegRuntimeLibrary,
        symbol_name: &[u8],
        diagnostic_symbol_name: &'static str,
    ) -> Result<u32, FfmpegProbeFailure> {
        // SAFETY: signature сверена с FFmpeg C API: `unsigned name(void)`.
        let version_function =
            unsafe { library.get::<VersionFunction>(symbol_name) }.map_err(|error| {
                FfmpegProbeFailure::ProbeFailed {
                    step: "load-version-symbol",
                    details: format!(
                        "{} missing in {}: {error}",
                        diagnostic_symbol_name,
                        runtime_library.diagnostic_name()
                    ),
                }
            })?;

        // SAFETY: symbol загружен из живой `Library`; функция не требует аргументов.
        Ok(unsafe { version_function() })
    }

    /// Проверяет, что libavcodec умеет найти базовый software decoder entry.
    fn require_decoder_api(library: &Library) -> Result<(), FfmpegProbeFailure> {
        // SAFETY: signature сверена с FFmpeg C API: `const AVCodec *fn(enum AVCodecID)`.
        let find_decoder = unsafe { library.get::<FindDecoderFunction>(b"avcodec_find_decoder\0") }
            .map_err(|error| FfmpegProbeFailure::ProbeFailed {
                step: "load-codec-symbol",
                details: format!("avcodec_find_decoder missing in libavcodec: {error}"),
            })?;

        // SAFETY: `AV_CODEC_ID_H264` — stable enum value; null return is handled below.
        let decoder = unsafe { find_decoder(AV_CODEC_ID_H264) };

        if decoder.is_null() {
            Err(FfmpegProbeFailure::ProbeFailed {
                step: "codec-api",
                details: "avcodec_find_decoder(AV_CODEC_ID_H264) returned null".to_owned(),
            })
        } else {
            Ok(())
        }
    }

    /// Проверяет, что libavutil отдаёт descriptor для базового planar YUV format-а.
    fn require_pixel_format_api(library: &Library) -> Result<(), FfmpegProbeFailure> {
        // SAFETY: signature сверена с FFmpeg C API: `const AVPixFmtDescriptor *fn(enum AVPixelFormat)`.
        let pixel_format_descriptor =
            unsafe { library.get::<PixelFormatDescriptorFunction>(b"av_pix_fmt_desc_get\0") }
                .map_err(|error| FfmpegProbeFailure::ProbeFailed {
                    step: "load-pixel-format-symbol",
                    details: format!("av_pix_fmt_desc_get missing in libavutil: {error}"),
                })?;

        // SAFETY: `AV_PIX_FMT_YUV420P` — stable enum value; null return is handled below.
        let descriptor = unsafe { pixel_format_descriptor(AV_PIX_FMT_YUV420P) };

        if descriptor.is_null() {
            Err(FfmpegProbeFailure::ProbeFailed {
                step: "pixel-format-api",
                details: "av_pix_fmt_desc_get(AV_PIX_FMT_YUV420P) returned null".to_owned(),
            })
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn missing_runtime_library_maps_to_typed_failure_without_panic() {
            let failure = match load_library(
                FfmpegRuntimeLibrary::LibAvUtil,
                &["rustiplayer-definitely-missing-libavutil-for-probe-test"],
            ) {
                Ok(_) => panic!("missing runtime candidate unexpectedly loaded"),
                Err(failure) => failure,
            };

            assert!(matches!(
                failure,
                FfmpegProbeFailure::MissingRuntimeLibraries {
                    library: FfmpegRuntimeLibrary::LibAvUtil,
                    ..
                }
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_time_probe_reports_backend_id_without_runtime_calls() {
        let report = compile_time_probe();

        assert_eq!(report.backend_id(), FFMPEG_SOFTWARE_BACKEND_ID);
        assert_eq!(report.runtime_status(), &FfmpegRuntimeProbeStatus::NotRun);
    }

    #[test]
    fn packed_ffmpeg_version_splits_into_major_minor_micro() {
        let version = FfmpegLibraryVersion::from_packed(0x003c_1a65);

        assert_eq!(version.major(), 60);
        assert_eq!(version.minor(), 26);
        assert_eq!(version.micro(), 101);
        assert_eq!(version.display(), "60.26.101");
    }

    #[test]
    fn runtime_version_mapping_accepts_minimum_supported_versions() {
        let minimum = minimum_supported_versions();
        let status = classify_runtime_versions(minimum);

        assert!(matches!(status, FfmpegRuntimeProbeStatus::Available(_)));
    }

    #[test]
    fn runtime_version_mapping_rejects_too_old_avcodec() {
        let versions = FfmpegLibraryVersions::new(
            FfmpegLibraryVersion::new(61, 99, 99),
            MINIMUM_LIBAVUTIL_VERSION,
        );
        let status = classify_runtime_versions(versions);

        assert!(matches!(
            status,
            FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::TooOld { .. })
        ));
    }

    #[test]
    fn runtime_version_mapping_rejects_too_old_avutil() {
        let versions = FfmpegLibraryVersions::new(
            MINIMUM_LIBAVCODEC_VERSION,
            FfmpegLibraryVersion::new(59, 99, 99),
        );
        let status = classify_runtime_versions(versions);

        assert!(matches!(
            status,
            FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::TooOld { .. })
        ));
    }

    #[test]
    fn failure_diagnostic_codes_distinguish_required_outcomes() {
        let missing_runtime = FfmpegProbeFailure::MissingRuntimeLibraries {
            library: FfmpegRuntimeLibrary::LibAvCodec,
            details: "not found".to_owned(),
        };
        let too_old = FfmpegProbeFailure::TooOld {
            minimum: minimum_supported_versions(),
            found: FfmpegLibraryVersions::new(
                FfmpegLibraryVersion::new(61, 0, 0),
                FfmpegLibraryVersion::new(59, 0, 0),
            ),
        };
        let probe_failed = FfmpegProbeFailure::ProbeFailed {
            step: "codec-api",
            details: "decoder missing".to_owned(),
        };

        assert_eq!(FfmpegProbeFailure::NoBuild.diagnostic_code(), "no-build");
        assert_eq!(missing_runtime.diagnostic_code(), "missing-runtime-libs");
        assert_eq!(too_old.diagnostic_code(), "too-old");
        assert_eq!(probe_failed.diagnostic_code(), "probe-failed");
    }

    #[test]
    fn report_mapping_preserves_missing_runtime_failure() {
        let failure = FfmpegProbeFailure::MissingRuntimeLibraries {
            library: FfmpegRuntimeLibrary::LibAvUtil,
            details: "libavutil.so.60: not found".to_owned(),
        };
        let report = report_from_runtime_status(
            FfmpegBuildStatus::FeatureEnabled,
            FfmpegRuntimeProbeStatus::Unavailable(failure.clone()),
        );

        assert!(!report.is_available());
        assert_eq!(report.failure(), Some(&failure));
    }

    #[test]
    fn report_mapping_preserves_probe_failed_failure() {
        let failure = FfmpegProbeFailure::ProbeFailed {
            step: "pixel-format-api",
            details: "descriptor missing".to_owned(),
        };
        let report = report_from_runtime_status(
            FfmpegBuildStatus::FeatureEnabled,
            FfmpegRuntimeProbeStatus::Unavailable(failure.clone()),
        );

        assert!(!report.is_available());
        assert_eq!(report.failure(), Some(&failure));
    }

    #[cfg(not(feature = "ffmpeg"))]
    #[test]
    fn runtime_probe_without_feature_reports_no_build() {
        let report = probe_runtime_availability();

        assert_eq!(report.build_status(), FfmpegBuildStatus::FeatureDisabled);
        assert_eq!(
            report.failure().map(FfmpegProbeFailure::diagnostic_code),
            Some("no-build")
        );
    }
}
