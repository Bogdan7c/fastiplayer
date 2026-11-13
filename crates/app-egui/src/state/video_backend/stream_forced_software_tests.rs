//! Защёлка «поток увёл media на программный декод» (мост 16d1, решение 324).
//!
//! Поток, где качества чередуются «аппаратный тянет / не тянет», гонял декодер по кругу:
//! окно на каждое качество выбирает лучший декодер, а смена декодера перематывает назад,
//! до границы качества. Тесты закрепляют обе половины правила: когда защёлка ставится
//! (только вынужденный уход с аппаратного) и что она делает (программный до нового media).

use codec_core::{VideoCodec, VideoDecodeRequirement};
use fastiplayer_config::VideoBackendPreference;
use player_core::VideoBackendSelectionRequest;

use super::{
    PlayerVideoDecoderThreadConfig, VideoBackendKind, VideoPipelinePlan,
    hardware_could_not_decode_the_stream,
};
use crate::dma_buf_runtime_fallback::DmaBufRuntimeFallbackController;

/// Запрос player-а на декодер под H.264-поток.
fn request(decodable_by_active_backend: bool) -> VideoBackendSelectionRequest {
    VideoBackendSelectionRequest {
        requirement: VideoDecodeRequirement::new(VideoCodec::H264),
        decodable_by_active_backend,
    }
}

fn software_plan() -> VideoPipelinePlan {
    VideoPipelinePlan::FfmpegHostUpload {
        decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
    }
}

fn hardware_plan() -> VideoPipelinePlan {
    VideoPipelinePlan::VaapiDmaBuf {
        decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
    }
}

#[test]
fn only_a_forced_move_from_hardware_to_software_latches() {
    let hardware = Some(VideoBackendKind::HardwareZeroCopy);
    let software = Some(VideoBackendKind::FfmpegSoftware);

    // Аппаратный не тянет описание потока, подбор дал программный — вынужденный уход.
    assert!(hardware_could_not_decode_the_stream(
        hardware,
        software_plan(),
        &request(/* decodable_by_active_backend */ false),
    ));
    // Возврат к лучшему декодеру «по предпочтению» — не вынужденный уход.
    assert!(!hardware_could_not_decode_the_stream(
        hardware,
        software_plan(),
        &request(/* decodable_by_active_backend */ true),
    ));
    // С программного на аппаратный и программный → программный — не уход с аппаратного.
    assert!(!hardware_could_not_decode_the_stream(
        software,
        hardware_plan(),
        &request(/* decodable_by_active_backend */ false),
    ));
    assert!(!hardware_could_not_decode_the_stream(
        software,
        software_plan(),
        &request(/* decodable_by_active_backend */ false),
    ));
    // Декодера ещё не было (первый подбор media) — защёлке нечего фиксировать.
    assert!(!hardware_could_not_decode_the_stream(
        None,
        software_plan(),
        &request(/* decodable_by_active_backend */ false),
    ));
}

#[test]
fn the_latch_keeps_software_until_the_next_media() {
    let mut controller = DmaBufRuntimeFallbackController::default();
    assert_eq!(
        controller.effective_preference(VideoBackendPreference::Auto),
        VideoBackendPreference::Auto,
        "precondition: no latch"
    );

    controller.latch_software_for_this_media(VideoBackendPreference::Auto);

    assert_eq!(
        controller.effective_preference(VideoBackendPreference::Auto),
        VideoBackendPreference::Software,
        "no return to hardware in this media"
    );
    // Новое предпочтение пользователя главнее защёлки, как и у фолбэка.
    assert_eq!(
        controller.effective_preference(VideoBackendPreference::Dynamic),
        VideoBackendPreference::Dynamic
    );
    // Повторная защёлка не переписывает первую.
    controller.latch_software_for_this_media(VideoBackendPreference::Dynamic);
    assert_eq!(
        controller.effective_preference(VideoBackendPreference::Auto),
        VideoBackendPreference::Software
    );
    // Защёлка не тратит единственную попытку фолбэка DMA-BUF/«не успевает».
    assert!(!controller.has_attempted_switch());

    controller.reset_for_new_media();

    assert_eq!(
        controller.effective_preference(VideoBackendPreference::Auto),
        VideoBackendPreference::Auto,
        "the next media starts with hardware again"
    );
}
