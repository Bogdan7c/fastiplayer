#!/usr/bin/env bash
# Локальный runner для реальных media regressions; default cargo test его не вызывает.

set -Eeuo pipefail

readonly SUCCESS_EXIT_CODE=0
readonly FAILURE_EXIT_CODE=1

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"
readonly SCRIPT_DIRECTORY="${script_directory}"
readonly REPO_ROOT="${repo_root}"

scenario_name=""
selected_path=""

print_help() {
    cat <<'EOF'
Usage: scripts/media-regression.sh --scenario NAME --path /absolute/or/relative/file

Runs exactly one explicitly selected real-media regression. The runner never searches
test-assets and never supplies a default filename.

Outcome contract:
  no selection                       NOT RUN: missing selection (exit 0)
  --scenario without its --path      NOT RUN: missing selection (exit non-zero)
  unreadable/wrong/assertion failure FAILED (exit non-zero)
  selected assertion succeeds        PASSED (exit 0)

Options:
  --scenario NAME  Required regression scenario; see --list-scenarios.
  --path FILE      Required local regular file for the selected scenario.
  --list-scenarios Print required properties for every scenario.
  --help           Show this help.
EOF
}

print_scenarios() {
    cat <<'EOF'
Scenario                       Required selected file properties
--------                       ---------------------------------
h264-avcc                      ISO BMFF H.264 with avcC SPS/PPS.
h264-keyframes                 H.264 stream containing key and inter frames.
h264-bframes-pts-dts           ISO BMFF H.264 B-frames with distinct PTS/DTS.
h264-ts-pts-only-ffmpeg        MPEG-TS H.264 без B-frames, минимум три PES с PTS и без DTS.
h264-signed-ctts               ISO BMFF H.264 signed-ctts regression near startup.
h264-startup-decode-point      H.264 stream with a startup decode point near zero.
h264-mkv-cue                   Matroska H.264 with usable nearby cues, at least 10 s.
h265-mov-sync-sample           ISO BMFF H.265 with a sync sample before 8 s.
h265-hvcc                      ISO BMFF H.265 with hvcC codec private data.
h265-mkv-cue                   Matroska H.265 with usable nearby cues, at least 10 s.
vp9-decode-point               WebM/Matroska VP9 with a keyframe near 66.93 s.
audio-decode-seek              Decodable audio with duration for a middle accurate seek.
audio-eof-replay               Decodable audio short enough to drain to EOF.
audio-unseekable-eof-seek      Decodable audio with an extension that can be opened as a stream.
audio-matroska-end-seek        Matroska/WebM Opus with public non-zero duration.
audio-matroska-late-seeks      Matroska/WebM Opus longer than 8 s with late seek coverage.
audio-wavpack-unsupported      Standalone WavPack expected to remain unsupported.
direct-http-range              MP4, MOV, MKV, or WebM readable through HTTP Range.
EOF
}

print_not_run_missing_selection() {
    printf 'NOT RUN: missing selection\n' >&2
}

print_failed() {
    local message="$1"
    printf 'FAILED: scenario=%s; path=%s; reason=%s\n' "${scenario_name}" "${selected_path}" "${message}" >&2
}

parse_arguments() {
    if (($# == 0)); then
        return
    fi

    while (($# > 0)); do
        case "$1" in
            --help)
                print_help
                exit "${SUCCESS_EXIT_CODE}"
                ;;
            --list-scenarios)
                print_scenarios
                exit "${SUCCESS_EXIT_CODE}"
                ;;
            --scenario)
                if (($# < 2)); then
                    scenario_name="<missing>"
                    return
                fi
                scenario_name="$2"
                shift 2
                ;;
            --path)
                if (($# < 2)); then
                    selected_path="<missing>"
                    return
                fi
                selected_path="$2"
                shift 2
                ;;
            *)
                print_failed "unknown argument '$1'"
                exit "${FAILURE_EXIT_CODE}"
                ;;
        esac
    done
}

validate_selection() {
    if [[ -z "${scenario_name}" && -z "${selected_path}" ]]; then
        print_not_run_missing_selection
        exit "${SUCCESS_EXIT_CODE}"
    fi

    if [[ -z "${scenario_name}" || -z "${selected_path}" || "${scenario_name}" == "<missing>" || "${selected_path}" == "<missing>" ]]; then
        print_not_run_missing_selection
        exit "${FAILURE_EXIT_CODE}"
    fi

    if [[ ! -f "${selected_path}" ]]; then
        print_failed "selected path is not a readable regular file"
        exit "${FAILURE_EXIT_CODE}"
    fi

    local selected_directory
    selected_directory="$(cd -- "$(dirname -- "${selected_path}")" && pwd -P)"
    selected_path="${selected_directory}/$(basename -- "${selected_path}")"
}

scenario_test_command() {
    case "${scenario_name}" in
        h264-avcc) printf '%s\n' 'symphonia-demux|h264_fixtures|h264_avcc_codec_private_is_present' ;;
        h264-keyframes) printf '%s\n' 'symphonia-demux|h264_fixtures|h264_packets_have_codec_aware_keyframe_states' ;;
        h264-bframes-pts-dts) printf '%s\n' 'symphonia-demux|h264_fixtures|h264_bframes_keep_presentation_pts_and_decode_dts' ;;
        h264-ts-pts-only-ffmpeg) printf '%s\n' 'video-ffmpeg|pts_only_mpeg_ts|pts_only_mpeg_ts_materializes_increasing_frames_after_start_and_seek' ;;
        h264-signed-ctts) printf '%s\n' 'symphonia-demux|h264_fixtures|h264_signed_ctts_offsets_do_not_wrap_pts' ;;
        h264-startup-decode-point) printf '%s\n' 'symphonia-demux|h264_fixtures|h264_startup_decode_point_accepts_first_keyframe' ;;
        h264-mkv-cue) printf '%s\n' 'symphonia-demux|h264_fixtures|h264_matroska_cue_seek_uses_near_decode_anchor' ;;
        h265-mov-sync-sample) printf '%s\n' 'symphonia-demux|h265_fixtures|h265_iso_bmff_decode_point_before_starts_on_sync_sample' ;;
        h265-hvcc) printf '%s\n' 'symphonia-demux|h265_fixtures|h265_iso_bmff_track_exposes_hvcc_codec_private' ;;
        h265-mkv-cue) printf '%s\n' 'symphonia-demux|h265_fixtures|h265_matroska_cue_seek_uses_near_decode_anchor' ;;
        vp9-decode-point) printf '%s\n' 'symphonia-demux|vp9_fixtures|vp9_decode_point_before_seek_reaches_near_target_keyframe' ;;
        audio-decode-seek) printf '%s\n' 'symphonia-demux|audio_fixture_decode_seek|audio_decode_and_middle_seek_preserve_decodable_pcm' ;;
        audio-eof-replay) printf '%s\n' 'symphonia-demux|audio_fixture_decode_seek|audio_eof_replay_returns_first_selected_audio_packet' ;;
        audio-unseekable-eof-seek) printf '%s\n' 'symphonia-demux|audio_fixture_decode_seek|unseekable_selected_audio_stream_after_eof_stays_unseekable' ;;
        audio-matroska-end-seek) printf '%s\n' 'symphonia-demux|audio_fixture_decode_seek|matroska_opus_end_seek_returns_audio_packet' ;;
        audio-matroska-late-seeks) printf '%s\n' 'symphonia-demux|audio_fixture_decode_seek|matroska_opus_aggressive_late_seeks_reach_near_target_packets' ;;
        audio-wavpack-unsupported) printf '%s\n' 'symphonia-demux|audio_fixture_decode_seek|wavpack_remains_explicitly_unsupported' ;;
        direct-http-range) printf '%s\n' 'service-direct-media|lib|tests::selected_media_opens_over_direct_http_range' ;;
        *) return 1 ;;
    esac
}

run_selected_test() {
    local command_spec="$1"
    local package_name
    local test_target
    local test_name
    local -a package_feature_arguments=()
    IFS='|' read -r package_name test_target test_name <<<"${command_spec}"

    # Реальный software decode test компилируется только при explicit FFmpeg feature.
    if [[ "${package_name}" == "video-ffmpeg" ]]; then
        package_feature_arguments=(--features ffmpeg)
    fi

    printf 'RUN: scenario=%s; path=%s\n' "${scenario_name}" "${selected_path}" >&2
    if [[ "${test_target}" == "lib" ]]; then
        if ! env \
            "RUSTIPLAYER_MEDIA_PATH=${selected_path}" \
            "RUSTIPLAYER_MEDIA_SCENARIO=${scenario_name}" \
            cargo +1.96.0 test -p "${package_name}" "${package_feature_arguments[@]}" --locked --lib "${test_name}" -- --ignored --exact --nocapture; then
            print_failed "selected assertion failed"
            exit "${FAILURE_EXIT_CODE}"
        fi
        return
    fi

    if ! env \
        "RUSTIPLAYER_MEDIA_PATH=${selected_path}" \
        "RUSTIPLAYER_MEDIA_SCENARIO=${scenario_name}" \
        cargo +1.96.0 test -p "${package_name}" "${package_feature_arguments[@]}" --locked --test "${test_target}" "${test_name}" -- --ignored --exact --nocapture; then
        print_failed "selected assertion failed"
        exit "${FAILURE_EXIT_CODE}"
    fi
}

run_inspection() {
    # Symphonia inspector не владеет MPEG-TS path-ом; этот asset проверяет сам production TS demuxer.
    if [[ "${scenario_name}" == "audio-wavpack-unsupported" || "${scenario_name}" == "h264-ts-pts-only-ffmpeg" ]]; then
        return
    fi

    if ! env \
        "RUSTIPLAYER_MEDIA_PATH=${selected_path}" \
        "RUSTIPLAYER_MEDIA_SCENARIO=${scenario_name}" \
        cargo +1.96.0 test -p symphonia-demux --locked --test manual_media_inspection selected_media_is_openable_and_reports_detected_tracks -- --ignored --exact --nocapture; then
        print_failed "selected file could not be inspected as media"
        exit "${FAILURE_EXIT_CODE}"
    fi
}

main() {
    parse_arguments "$@"
    validate_selection
    cd "${REPO_ROOT}"

    local command_spec
    if ! command_spec="$(scenario_test_command)"; then
        print_failed "unknown scenario"
        exit "${FAILURE_EXIT_CODE}"
    fi

    run_inspection
    run_selected_test "${command_spec}"
    printf 'PASSED: scenario=%s; path=%s\n' "${scenario_name}" "${selected_path}" >&2
}

main "$@"
