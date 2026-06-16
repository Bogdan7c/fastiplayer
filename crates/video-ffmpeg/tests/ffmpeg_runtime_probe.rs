use video_ffmpeg::{
    FfmpegBuildStatus, FfmpegProbeFailure, FfmpegRuntimeProbeStatus, probe_runtime_availability,
};

#[test]
#[ignore = "requires video-ffmpeg --features ffmpeg and installed FFmpeg runtime libraries"]
fn installed_ffmpeg_runtime_probe_reports_available_runtime() {
    let report = probe_runtime_availability();

    assert_eq!(report.build_status(), expected_build_status());

    match report.runtime_status() {
        FfmpegRuntimeProbeStatus::Available(runtime_info) => {
            assert!(runtime_info.versions().avcodec().major() >= 62);
            assert!(runtime_info.versions().avutil().major() >= 60);
        }
        FfmpegRuntimeProbeStatus::Unavailable(FfmpegProbeFailure::NoBuild) => {
            panic!("run with `cargo test -p video-ffmpeg --features ffmpeg -- --ignored`")
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
