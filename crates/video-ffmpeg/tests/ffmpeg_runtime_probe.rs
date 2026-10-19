// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

use video_ffmpeg::{
    FfmpegBuildStatus, FfmpegProbeFailure, FfmpegRuntimeProbeStatus, probe_runtime_availability,
};

#[test]
#[ignore = "requires video-ffmpeg --features ffmpeg (system or vendored FFmpeg 8.1+)"]
fn installed_ffmpeg_runtime_probe_reports_available_runtime() {
    let report = probe_runtime_availability();

    assert_eq!(report.build_status(), expected_build_status());

    match report.runtime_status() {
        FfmpegRuntimeProbeStatus::Available(runtime_info) => {
            assert!(runtime_info.versions().avcodec().major() >= 62);
            assert!(runtime_info.versions().avutil().major() >= 60);
        }
        FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::NoBuild) => {
            panic!(
                "run with `cargo test -p video-ffmpeg --features ffmpeg --test ffmpeg_runtime_probe -- --ignored --exact installed_ffmpeg_runtime_probe_reports_available_runtime`"
            )
        }
        FfmpegRuntimeProbeStatus::Unavailable(failure) => {
            panic!("installed FFmpeg runtime probe failed: {failure:?}")
        }
        FfmpegRuntimeProbeStatus::NotRun => panic!("runtime probe must execute in this test"),
    }
}

fn expected_build_status() -> FfmpegBuildStatus {
    if cfg!(feature = "ffmpeg") {
        FfmpegBuildStatus::FeatureEnabled
    } else {
        FfmpegBuildStatus::FeatureDisabled
    }
}

/// Vendored-сборка: процесс обязан работать именно с закреплённым FFmpeg 8.1.3,
/// а не с системным (на машине разработчика это может быть FFmpeg 9, в CI —
/// FFmpeg 6.1). Версии библиотек FFmpeg 8.1.3: libavcodec 62.28.103,
/// libavutil 60.26.103. При обновлении закреплённой версии в `ffmpeg-vendor`
/// тест нужно обновить осознанно.
#[cfg(feature = "vendored-ffmpeg")]
#[test]
fn vendored_ffmpeg_probe_reports_the_pinned_libraries_not_the_system_ones() {
    let report = probe_runtime_availability();

    let FfmpegRuntimeProbeStatus::Available(runtime_info) = report.runtime_status() else {
        panic!(
            "vendored FFmpeg must always be available, got {:?}",
            report.runtime_status()
        );
    };
    assert_eq!(runtime_info.versions().avcodec().display(), "62.28.103");
    assert_eq!(runtime_info.versions().avutil().display(), "60.26.103");
}
