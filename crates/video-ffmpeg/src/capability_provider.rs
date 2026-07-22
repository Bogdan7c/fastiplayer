//! Capability provider для FFmpeg software decode path.
//!
//! Этот модуль не запускает playback pipeline. Он только превращает runtime
//! probe FFmpeg в neutral `capability-core` report: raw outputs появляются
//! после успешного probe, а renderer intersection остаётся задачей scanner-а.

use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SupportedVideoOutput,
    VideoCapabilityProvider,
};
use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, H265Profile,
    SupportedVideoDecodeFormat, VideoProfile, Vp8Profile, Vp9Profile,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

use crate::probe::{
    FfmpegProbeFailure, FfmpegProbeReport, FfmpegRuntimeProbeStatus, probe_runtime_availability,
};
use crate::{FFMPEG_SOFTWARE_BACKEND_ID, ffmpeg_software_backend_id};

/// Human-readable имя backend-а в capability reports.
const FFMPEG_SOFTWARE_DISPLAY_NAME: &str = "FFmpeg software";

/// Runtime probe boundary: в production это `probe_runtime_availability()`, в tests - stub.
type RuntimeProbeFn = fn() -> FfmpegProbeReport;

/// Provider software FFmpeg capabilities для `capability-core`.
#[derive(Debug, Clone, Copy)]
pub struct FfmpegSoftwareCapabilityProvider {
    /// Probe injected as function pointer, чтобы tests не зависели от installed FFmpeg.
    runtime_probe: RuntimeProbeFn,
}

impl FfmpegSoftwareCapabilityProvider {
    /// Создаёт provider, который использует реальный runtime FFmpeg probe.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            runtime_probe: probe_runtime_availability,
        }
    }

    /// Создаёт provider с deterministic probe для focused unit tests.
    #[cfg(test)]
    const fn with_runtime_probe(runtime_probe: RuntimeProbeFn) -> Self {
        Self { runtime_probe }
    }
}

impl Default for FfmpegSoftwareCapabilityProvider {
    /// Default provider сохраняет production probe policy.
    fn default() -> Self {
        Self::new()
    }
}

impl VideoCapabilityProvider for FfmpegSoftwareCapabilityProvider {
    /// Возвращает canonical id FFmpeg software backend-а без runtime calls.
    fn backend_id(&self) -> DecodeBackendId {
        ffmpeg_software_backend_id()
    }

    /// Выполняет runtime probe и отдаёт raw software outputs только для доступного FFmpeg.
    fn probe(&self) -> BackendCapabilities {
        probe_ffmpeg_software_capabilities_with(self.runtime_probe)
    }
}

/// Выполняет FFmpeg software capability probe через production runtime probe.
#[must_use]
pub fn probe_ffmpeg_software_capabilities() -> BackendCapabilities {
    probe_ffmpeg_software_capabilities_with(probe_runtime_availability)
}

/// Собирает `BackendCapabilities` из injectable runtime probe boundary.
fn probe_ffmpeg_software_capabilities_with(runtime_probe: RuntimeProbeFn) -> BackendCapabilities {
    let probe_report = runtime_probe();
    let backend_id = ffmpeg_software_backend_id();

    if !probe_report.is_available() {
        return unavailable_backend_report(backend_id, &probe_report);
    }

    let runtime_versions = match probe_report.runtime_status() {
        FfmpegRuntimeProbeStatus::Available(runtime_info) => runtime_info.versions(),
        FfmpegRuntimeProbeStatus::NotRun | FfmpegRuntimeProbeStatus::Unavailable(_) => {
            return unavailable_backend_report(backend_id, &probe_report);
        }
    };

    let raw_supported_outputs = ffmpeg_software_supported_outputs(backend_id.clone());

    BackendCapabilities {
        backend_id,
        display_name: FFMPEG_SOFTWARE_DISPLAY_NAME.to_string(),
        status: BackendProbeStatus::Available,
        driver: BackendDriverInfo {
            vendor: Some("FFmpeg".to_string()),
            driver_name: Some(format!(
                "libavcodec {} / libavutil {}",
                runtime_versions.avcodec().display(),
                runtime_versions.avutil().display()
            )),
            device_name: None,
        },
        raw_supported_outputs,
        raw_profiles: ffmpeg_software_profile_labels(),
        raw_entrypoints: vec!["software-decode".to_string()],
        raw_rt_formats: ffmpeg_software_rt_format_labels(),
        quirks: Vec::new(),
        diagnostics: vec![format!(
            "{FFMPEG_SOFTWARE_BACKEND_ID}: runtime probe passed; raw outputs require renderer SoftwareHostUpload intersection"
        )],
    }
}

/// Формирует unavailable report и сохраняет typed FFmpeg failure в diagnostics/reason.
fn unavailable_backend_report(
    backend_id: DecodeBackendId,
    probe_report: &FfmpegProbeReport,
) -> BackendCapabilities {
    let reason = unavailable_reason(probe_report);
    let mut capabilities =
        BackendCapabilities::unavailable(backend_id, FFMPEG_SOFTWARE_DISPLAY_NAME, reason.clone());
    capabilities.diagnostics.push(reason);
    capabilities
}

/// Переводит typed probe failure в stable human-readable diagnostic.
fn unavailable_reason(probe_report: &FfmpegProbeReport) -> String {
    match probe_report.runtime_status() {
        FfmpegRuntimeProbeStatus::Available(_) => {
            "FFmpeg runtime probe unexpectedly reported available at unavailable boundary"
                .to_string()
        }
        FfmpegRuntimeProbeStatus::NotRun => {
            "FFmpeg runtime probe did not run, so software outputs are not registered".to_string()
        }
        FfmpegRuntimeProbeStatus::Unavailable(failure) => ffmpeg_probe_failure_reason(failure),
    }
}

/// Детализирует причину недоступности без раскрытия raw FFmpeg pointers.
fn ffmpeg_probe_failure_reason(failure: &FfmpegProbeFailure) -> String {
    match failure {
        FfmpegProbeFailure::NoBuild => {
            "FFmpeg software backend unavailable (no-build): crate built without feature `ffmpeg`"
                .to_string()
        }
        FfmpegProbeFailure::MissingRuntimeLibraries { library, details } => format!(
            "FFmpeg software backend unavailable ({}): missing {}, {details}",
            failure.diagnostic_code(),
            library.diagnostic_name()
        ),
        FfmpegProbeFailure::TooOld { minimum, found } => format!(
            "FFmpeg software backend unavailable ({}): need libavcodec >= {} and libavutil >= {}, found libavcodec {} and libavutil {}",
            failure.diagnostic_code(),
            minimum.avcodec().display(),
            minimum.avutil().display(),
            found.avcodec().display(),
            found.avutil().display()
        ),
        FfmpegProbeFailure::ProbeFailed { step, details } => format!(
            "FFmpeg software backend unavailable ({}): {step} failed: {details}",
            failure.diagnostic_code()
        ),
    }
}

/// Собирает raw software outputs для supported v1 FFmpeg software matrix.
fn ffmpeg_software_supported_outputs(backend_id: DecodeBackendId) -> Vec<SupportedVideoOutput> {
    ffmpeg_software_output_specs()
        .iter()
        .copied()
        .map(|specification| specification.into_supported_output(backend_id.clone()))
        .collect()
}

/// Возвращает profile labels, которые provider объявляет после successful probe.
fn ffmpeg_software_profile_labels() -> Vec<String> {
    let mut labels = ffmpeg_software_output_specs()
        .iter()
        .map(|specification| specification.profile.to_string())
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    labels
}

/// Возвращает raw software pixel-layout labels для diagnostics.
fn ffmpeg_software_rt_format_labels() -> Vec<String> {
    let mut labels = ffmpeg_software_output_specs()
        .iter()
        .map(|specification| specification.pixel_layout.diagnostic_label().to_string())
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    labels
}

/// Static entry в provider-declared software matrix.
#[derive(Debug, Clone, Copy)]
struct FfmpegSoftwareOutputSpec {
    /// Codec-specific profile, которому принадлежит output.
    profile: VideoProfile,

    /// Bit depth decoded planes.
    bit_depth: BitDepth,

    /// Chroma subsampling decoded planes.
    chroma: ChromaSubsampling,

    /// Exact host-planar frame layout, который отдаёт decoder.
    pixel_layout: VideoFramePixelLayout,

    /// Может ли output принять HDR input metadata/pixels.
    hdr_input: bool,
}

impl FfmpegSoftwareOutputSpec {
    /// Превращает static spec в neutral capability output.
    fn into_supported_output(self, backend_id: DecodeBackendId) -> SupportedVideoOutput {
        let frame_contract = self.frame_contract();
        debug_assert!(frame_contract.validate().is_ok());

        SupportedVideoOutput {
            backend: backend_id,
            decode_format: SupportedVideoDecodeFormat {
                codec: self.profile.codec(),
                profile: self.profile,
                bit_depth: self.bit_depth,
                chroma: self.chroma,
                max_width: None,
                max_height: None,
                max_fps: None,
                hdr_input: self.hdr_input,
            },
            frame_contract,
        }
    }

    /// Возвращает exact SoftwareHostUpload contract для decoded layout-а.
    const fn frame_contract(self) -> VideoFrameContract {
        VideoFrameContract {
            pixel_layout: self.pixel_layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        }
    }
}

/// Описывает v1 software decode matrix без обращения к renderer capabilities.
fn ffmpeg_software_output_specs() -> &'static [FfmpegSoftwareOutputSpec] {
    FFMPEG_SOFTWARE_OUTPUT_SPECS
}

/// Единый source of truth для raw software outputs, profile labels и layout diagnostics.
const FFMPEG_SOFTWARE_OUTPUT_SPECS: &[FfmpegSoftwareOutputSpec] = &[
    output_spec(
        VideoProfile::H264(H264Profile::Baseline),
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar8,
    ),
    output_spec(
        VideoProfile::H264(H264Profile::ConstrainedBaseline),
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar8,
    ),
    output_spec(
        VideoProfile::H264(H264Profile::Main),
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar8,
    ),
    output_spec(
        VideoProfile::H264(H264Profile::High),
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar8,
    ),
    output_spec(
        VideoProfile::Vp8(Vp8Profile::Version0To3),
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar8,
    ),
    output_spec(
        VideoProfile::H265(H265Profile::Main),
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar8,
    ),
    output_spec(
        VideoProfile::H265(H265Profile::Main10),
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar10Le,
    ),
    output_spec(
        VideoProfile::H265(H265Profile::Main12),
        BitDepth::Twelve,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar12Le,
    ),
    output_spec(
        VideoProfile::H265(H265Profile::Main422_10),
        BitDepth::Ten,
        ChromaSubsampling::Yuv422,
        VideoFramePixelLayout::Yuv422Planar10Le,
    ),
    output_spec(
        VideoProfile::H265(H265Profile::Main422_12),
        BitDepth::Twelve,
        ChromaSubsampling::Yuv422,
        VideoFramePixelLayout::Yuv422Planar12Le,
    ),
    output_spec(
        VideoProfile::H265(H265Profile::Main444),
        BitDepth::Eight,
        ChromaSubsampling::Yuv444,
        VideoFramePixelLayout::Yuv444Planar8,
    ),
    output_spec(
        VideoProfile::H265(H265Profile::Main444_10),
        BitDepth::Ten,
        ChromaSubsampling::Yuv444,
        VideoFramePixelLayout::Yuv444Planar10Le,
    ),
    output_spec(
        VideoProfile::Vp9(Vp9Profile::Profile0),
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar8,
    ),
    output_spec(
        VideoProfile::Vp9(Vp9Profile::Profile1),
        BitDepth::Eight,
        ChromaSubsampling::Yuv422,
        VideoFramePixelLayout::Yuv422Planar8,
    ),
    output_spec(
        VideoProfile::Vp9(Vp9Profile::Profile1),
        BitDepth::Eight,
        ChromaSubsampling::Yuv444,
        VideoFramePixelLayout::Yuv444Planar8,
    ),
    output_spec(
        VideoProfile::Vp9(Vp9Profile::Profile2),
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar10Le,
    ),
    output_spec(
        VideoProfile::Vp9(Vp9Profile::Profile2),
        BitDepth::Twelve,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar12Le,
    ),
    output_spec(
        VideoProfile::Vp9(Vp9Profile::Profile3),
        BitDepth::Ten,
        ChromaSubsampling::Yuv422,
        VideoFramePixelLayout::Yuv422Planar10Le,
    ),
    output_spec(
        VideoProfile::Vp9(Vp9Profile::Profile3),
        BitDepth::Twelve,
        ChromaSubsampling::Yuv422,
        VideoFramePixelLayout::Yuv422Planar12Le,
    ),
    output_spec(
        VideoProfile::Vp9(Vp9Profile::Profile3),
        BitDepth::Ten,
        ChromaSubsampling::Yuv444,
        VideoFramePixelLayout::Yuv444Planar10Le,
    ),
    output_spec(
        VideoProfile::Av1(Av1Profile::Main),
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar8,
    ),
    output_spec(
        VideoProfile::Av1(Av1Profile::Main),
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
        VideoFramePixelLayout::Yuv420Planar10Le,
    ),
    output_spec(
        VideoProfile::Av1(Av1Profile::High),
        BitDepth::Eight,
        ChromaSubsampling::Yuv444,
        VideoFramePixelLayout::Yuv444Planar8,
    ),
    output_spec(
        VideoProfile::Av1(Av1Profile::High),
        BitDepth::Ten,
        ChromaSubsampling::Yuv444,
        VideoFramePixelLayout::Yuv444Planar10Le,
    ),
];

/// Создаёт spec и централизует HDR policy для high-bit software formats.
const fn output_spec(
    profile: VideoProfile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
    pixel_layout: VideoFramePixelLayout,
) -> FfmpegSoftwareOutputSpec {
    FfmpegSoftwareOutputSpec {
        profile,
        bit_depth,
        chroma,
        pixel_layout,
        hdr_input: matches!(bit_depth, BitDepth::Ten | BitDepth::Twelve),
    }
}

#[cfg(test)]
mod tests {
    use capability_core::VideoCapabilityProvider;

    use crate::probe::{
        FfmpegBuildStatus, FfmpegLibraryVersion, FfmpegLibraryVersions, FfmpegRuntimeInfo,
        FfmpegRuntimeLibrary, minimum_supported_versions, report_from_runtime_status,
    };

    use super::*;

    /// Возвращает synthetic successful probe без зависимости от installed FFmpeg.
    fn successful_probe() -> FfmpegProbeReport {
        report_from_runtime_status(
            FfmpegBuildStatus::FeatureEnabled,
            FfmpegRuntimeProbeStatus::Available(FfmpegRuntimeInfo::new(
                minimum_supported_versions(),
            )),
        )
    }

    /// Возвращает synthetic missing-library probe для unavailable diagnostics.
    fn missing_runtime_probe() -> FfmpegProbeReport {
        report_from_runtime_status(
            FfmpegBuildStatus::FeatureEnabled,
            FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::MissingRuntimeLibraries {
                library: FfmpegRuntimeLibrary::LibAvCodec,
                details: "libavcodec.so was not found".to_string(),
            }),
        )
    }

    /// Возвращает synthetic too-old probe для unavailable diagnostics.
    fn too_old_probe() -> FfmpegProbeReport {
        report_from_runtime_status(
            FfmpegBuildStatus::FeatureEnabled,
            FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::TooOld {
                minimum: minimum_supported_versions(),
                found: FfmpegLibraryVersions::new(
                    FfmpegLibraryVersion::new(61, 0, 0),
                    FfmpegLibraryVersion::new(59, 0, 0),
                ),
            }),
        )
    }

    /// Проверяет, что successful probe регистрирует raw software outputs.
    #[test]
    fn probe_success_registers_raw_software_outputs() {
        let provider = FfmpegSoftwareCapabilityProvider::with_runtime_probe(successful_probe);
        let capabilities = provider.probe();

        assert_eq!(capabilities.backend_id, ffmpeg_software_backend_id());
        assert!(capabilities.status.is_available());
        assert!(!capabilities.raw_supported_outputs.is_empty());
        assert!(capabilities.raw_supported_outputs.iter().all(|output| {
            output.backend == ffmpeg_software_backend_id()
                && output.frame_contract.transfer_path == VideoFrameTransferPath::SoftwareHostUpload
                && output.frame_contract.pixel_layout.is_host_planar()
        }));
        assert!(capabilities.raw_supported_outputs.iter().any(|output| {
            output.decode_format.profile == VideoProfile::H265(H265Profile::Main422_12)
                && output.frame_contract.pixel_layout == VideoFramePixelLayout::Yuv422Planar12Le
        }));
    }

    #[test]
    fn probe_success_registers_exact_h264_baseline_software_output() {
        let provider = FfmpegSoftwareCapabilityProvider::with_runtime_probe(successful_probe);
        let capabilities = provider.probe();
        let baseline_output = capabilities
            .raw_supported_outputs
            .iter()
            .find(|output| {
                output.decode_format.profile == VideoProfile::H264(H264Profile::Baseline)
            })
            .expect("successful FFmpeg probe должен объявлять exact H.264 Baseline output");

        assert_eq!(
            baseline_output.decode_format.codec,
            codec_core::VideoCodec::H264
        );
        assert_eq!(baseline_output.decode_format.bit_depth, BitDepth::Eight);
        assert_eq!(
            baseline_output.decode_format.chroma,
            ChromaSubsampling::Yuv420
        );
        assert_eq!(
            baseline_output.frame_contract,
            VideoFrameContract::host_yuv420_planar8()
        );
        assert!(
            capabilities
                .raw_profiles
                .iter()
                .any(|label| label == "H.264 Baseline")
        );
    }

    /// Проверяет, что missing runtime не публикует raw outputs и сохраняет diagnostic.
    #[test]
    fn missing_ffmpeg_registers_unavailable_diagnostics() {
        let provider = FfmpegSoftwareCapabilityProvider::with_runtime_probe(missing_runtime_probe);
        let capabilities = provider.probe();

        assert!(!capabilities.status.is_available());
        assert!(capabilities.raw_supported_outputs.is_empty());
        assert!(
            capabilities
                .diagnostics
                .iter()
                .any(|message| message.contains("missing-runtime-libs"))
        );
    }

    /// Проверяет, что too-old runtime не публикует raw outputs и сохраняет version diagnostic.
    #[test]
    fn too_old_ffmpeg_registers_unavailable_diagnostics() {
        let provider = FfmpegSoftwareCapabilityProvider::with_runtime_probe(too_old_probe);
        let capabilities = provider.probe();

        assert!(!capabilities.status.is_available());
        assert!(capabilities.raw_supported_outputs.is_empty());
        assert!(
            capabilities
                .diagnostics
                .iter()
                .any(|message| message.contains("too-old"))
        );
    }
}
