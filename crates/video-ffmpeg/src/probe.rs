//! Compile-time FFmpeg availability scaffold.

use crate::FFMPEG_SOFTWARE_BACKEND_ID;

/// Build status, который не требует runtime FFmpeg calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegBuildStatus {
    /// Crate собран без optional FFmpeg raw binding dependency.
    FeatureDisabled,

    /// Crate собран с optional raw binding dependency.
    FeatureEnabled,
}

/// Минимальный probe report для будущего capability scanner-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegProbeReport {
    /// Canonical backend id для diagnostic/capability layers.
    backend_id: &'static str,

    /// Compile-time status optional FFmpeg feature-а.
    build_status: FfmpegBuildStatus,
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
}

/// Возвращает только compile-time status; runtime probe появится отдельной сессией.
#[must_use]
pub const fn compile_time_probe() -> FfmpegProbeReport {
    FfmpegProbeReport {
        backend_id: FFMPEG_SOFTWARE_BACKEND_ID,
        build_status: current_build_status(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_time_probe_reports_backend_id() {
        let report = compile_time_probe();

        assert_eq!(report.backend_id(), FFMPEG_SOFTWARE_BACKEND_ID);
    }
}
