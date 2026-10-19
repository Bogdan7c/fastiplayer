use crate::FFMPEG_SOFTWARE_BACKEND_ID;
#[cfg(any(feature = "ffmpeg", test))]
use crate::ffi::runtime_version::LinkedFfmpegRuntime;

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

/// Опрашивает FFmpeg, с которым процесс реально слинкован.
///
/// Раньше здесь был `dlopen` по списку имён `.so`: при жёсткой линковке он
/// ничего не добавлял (без библиотеки процесс не стартует), зато мог найти
/// чужой FFmpeg — например, системный при нашем vendored — и сообщить его версию.
#[cfg(feature = "ffmpeg")]
fn execute_runtime_probe() -> FfmpegRuntimeProbeStatus {
    classify_linked_runtime(crate::ffi::runtime_version::linked_ffmpeg_runtime())
}

/// Классифицирует снимок прилинкованного FFmpeg в typed runtime status.
///
/// Сначала базовое API (иначе версии бессмысленны), затем минимальные версии.
#[cfg(any(feature = "ffmpeg", test))]
fn classify_linked_runtime(linked_runtime: LinkedFfmpegRuntime) -> FfmpegRuntimeProbeStatus {
    if !linked_runtime.has_h264_decoder {
        return FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::ProbeFailed {
            step: "codec-api",
            details: "avcodec_find_decoder(AV_CODEC_ID_H264) returned null".to_owned(),
        });
    }
    if !linked_runtime.has_yuv420p_descriptor {
        return FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::ProbeFailed {
            step: "pixel-format-api",
            details: "av_pix_fmt_desc_get(AV_PIX_FMT_YUV420P) returned null".to_owned(),
        });
    }

    classify_runtime_versions(FfmpegLibraryVersions::new(
        FfmpegLibraryVersion::from_packed(linked_runtime.avcodec_packed_version),
        FfmpegLibraryVersion::from_packed(linked_runtime.avutil_packed_version),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_time_probe_reports_backend_id_without_runtime_calls() {
        let report = compile_time_probe();

        assert_eq!(report.backend_id(), "ffmpeg-sw");
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
        assert_eq!(too_old.diagnostic_code(), "too-old");
        assert_eq!(probe_failed.diagnostic_code(), "probe-failed");
    }

    /// Снимок «здорового» FFmpeg 8.1.3 (libavcodec 62.28.103, libavutil 60.26.103).
    fn healthy_linked_runtime() -> LinkedFfmpegRuntime {
        LinkedFfmpegRuntime {
            avcodec_packed_version: (62 << 16) | (28 << 8) | 103,
            avutil_packed_version: (60 << 16) | (26 << 8) | 103,
            has_h264_decoder: true,
            has_yuv420p_descriptor: true,
        }
    }

    #[test]
    fn linked_runtime_with_supported_versions_is_available_with_exact_versions() {
        let status = classify_linked_runtime(healthy_linked_runtime());

        let FfmpegRuntimeProbeStatus::Available(runtime_info) = status else {
            panic!("healthy linked FFmpeg must be available, got {status:?}");
        };
        assert_eq!(runtime_info.versions().avcodec().display(), "62.28.103");
        assert_eq!(runtime_info.versions().avutil().display(), "60.26.103");
    }

    #[test]
    fn linked_runtime_without_h264_decoder_fails_codec_api_step() {
        let linked_runtime = LinkedFfmpegRuntime {
            has_h264_decoder: false,
            ..healthy_linked_runtime()
        };

        let status = classify_linked_runtime(linked_runtime);

        assert!(matches!(
            status,
            FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::ProbeFailed {
                step: "codec-api",
                ..
            })
        ));
    }

    #[test]
    fn linked_runtime_without_pixel_format_descriptor_fails_pixel_format_step() {
        let linked_runtime = LinkedFfmpegRuntime {
            has_yuv420p_descriptor: false,
            ..healthy_linked_runtime()
        };

        let status = classify_linked_runtime(linked_runtime);

        assert!(matches!(
            status,
            FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::ProbeFailed {
                step: "pixel-format-api",
                ..
            })
        ));
    }

    #[test]
    fn linked_old_system_ffmpeg_is_reported_too_old_with_found_versions() {
        // FFmpeg 6.1 (Ubuntu 24.04): libavcodec 60.31.102, libavutil 58.29.100.
        let linked_runtime = LinkedFfmpegRuntime {
            avcodec_packed_version: (60 << 16) | (31 << 8) | 102,
            avutil_packed_version: (58 << 16) | (29 << 8) | 100,
            ..healthy_linked_runtime()
        };

        let status = classify_linked_runtime(linked_runtime);

        let FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::TooOld { found, .. }) =
            status
        else {
            panic!("old linked FFmpeg must be too-old, got {status:?}");
        };
        assert_eq!(found.avcodec().display(), "60.31.102");
        assert_eq!(found.avutil().display(), "58.29.100");
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
