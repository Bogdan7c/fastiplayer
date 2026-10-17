//! Поля BSI/`audfrm` фикстур ffmpeg против опций кодера (`generate.sh`) и
//! `ffprobe -show_frames` (side data «downmix info»: уровни Lo/Ro и Lt/Rt).

use super::support::{EAC3_FIXTURES, eac3_fixture, split_frames};
use crate::syntax::annex_d::{
    AnalogDigitalConverterType, DolbyHeadphoneMode, DolbySurroundExMode, PreferredStereoDownmix,
};
use crate::syntax::bsi_values::{AudioCodingMode, DolbySurroundMode, RoomType};
use crate::syntax::eac3::bsi::StreamType;
use crate::syntax::eac3::bsi_metadata::MixingOption;
use crate::syntax::eac3::syncframe::{EnhancedSyncframeInfo, read_enhanced_syncframe_info};

fn first_info(name: &str) -> EnhancedSyncframeInfo {
    read_enhanced_syncframe_info(split_frames(eac3_fixture(name))[0]).unwrap()
}

/// Частота, режим, LFE, блоки и длина кадра (= битрейт · длительность кадра).
#[test]
fn stream_parameters_match_encoder_settings() {
    let expected: [(&str, u32, AudioCodingMode, bool, u32, usize); 11] = [
        (
            "mono-48k-64k.eac3",
            48_000,
            AudioCodingMode::Mono,
            false,
            64,
            6,
        ),
        (
            "stereo-48k-96k.eac3",
            48_000,
            AudioCodingMode::Stereo,
            false,
            96,
            6,
        ),
        (
            "stereo-44k1-192k.eac3",
            44_100,
            AudioCodingMode::Stereo,
            false,
            192,
            6,
        ),
        (
            "stereo-32k-640k.eac3",
            32_000,
            AudioCodingMode::Stereo,
            false,
            640,
            6,
        ),
        (
            "stereo-48k-128k-cpl-high.eac3",
            48_000,
            AudioCodingMode::Stereo,
            false,
            128,
            6,
        ),
        (
            "3f-48k-256k.eac3",
            48_000,
            AudioCodingMode::ThreeFront,
            false,
            256,
            6,
        ),
        (
            "2f1r-48k-192k.eac3",
            48_000,
            AudioCodingMode::StereoMonoSurround,
            false,
            192,
            6,
        ),
        (
            "quad-44k1-256k.eac3",
            44_100,
            AudioCodingMode::StereoStereoSurround,
            false,
            256,
            6,
        ),
        (
            "2p1-48k-192k.eac3",
            48_000,
            AudioCodingMode::Stereo,
            true,
            192,
            6,
        ),
        (
            "5p1-48k-384k.eac3",
            48_000,
            AudioCodingMode::ThreeFrontStereoSurround,
            true,
            384,
            6,
        ),
        // 1 Мбит/с при 32 кГц не помещается в 6 блоков (> 2048 слов): кодер
        // выбирает кадры по 3 блока (numblkscod = 2) и передаёт convsync.
        (
            "5p1-32k-1000k.eac3",
            32_000,
            AudioCodingMode::ThreeFrontStereoSurround,
            true,
            1000,
            3,
        ),
    ];
    assert_eq!(expected.len(), EAC3_FIXTURES.len());
    for (name, rate, mode, lfe_on, kbps, blocks) in expected {
        for frame in split_frames(eac3_fixture(name)) {
            let info = read_enhanced_syncframe_info(frame).unwrap();
            assert_eq!(info.stream_type, StreamType::Independent, "{name}");
            assert_eq!(info.substream_id, 0, "{name}");
            assert_eq!(info.bsi.bitstream_id, 16, "{name}");
            assert_eq!(info.sample_rate_hz, rate, "{name}");
            assert_eq!(info.bsi.audio_coding_mode, mode, "{name}");
            assert_eq!(info.bsi.lfe_on, lfe_on, "{name}");
            assert_eq!(info.audio_blocks, blocks, "{name}");
            assert_eq!(info.samples_per_channel, blocks * 256, "{name}");
            // Длина кадра: битрейт × (256·блоков / fs) / 8; у 44,1 кГц кодер
            // чередует длины (дробное число байт), поэтому допускаем ±2 байта.
            let exact = f64::from(kbps) * 1000.0 * (256 * blocks) as f64 / f64::from(rate) / 8.0;
            assert!(
                (info.frame_length_bytes as f64 - exact).abs() <= 2.0,
                "{name}: {}",
                info.frame_length_bytes
            );
            assert_eq!(
                info.bsi.converter_synchronization.is_some(),
                blocks != 6,
                "{name}"
            );
            assert!(!info.audio_frame.adaptive_hybrid_transform, "{name}");
        }
    }
}

#[test]
fn informational_metadata_matches_encoder_options() {
    let mono = first_info("mono-48k-64k.eac3");
    assert_eq!(mono.bsi.program.dialogue_normalization.code(), 27);
    let informational = mono.bsi.informational.unwrap();
    let production = informational.production.unwrap();
    assert_eq!(production.peak_mixing_level_db_spl, 105);
    assert_eq!(production.room_type, RoomType::SmallRoomFlat);
    assert_eq!(informational.source_sample_rate_doubled, Some(false));

    let stereo = first_info("stereo-48k-96k.eac3").bsi.informational.unwrap();
    assert_eq!(stereo.dolby_surround_mode, Some(DolbySurroundMode::Encoded));
    assert_eq!(
        stereo.dolby_headphone_mode,
        Some(DolbyHeadphoneMode::Encoded)
    );
    assert_eq!(stereo.dolby_surround_ex_mode, None);

    let marked = first_info("stereo-44k1-192k.eac3")
        .bsi
        .informational
        .unwrap();
    assert!(marked.copyright);
    assert!(!marked.original_bitstream);

    let surround = first_info("5p1-48k-384k.eac3").bsi.informational.unwrap();
    assert_eq!(
        surround.dolby_surround_ex_mode,
        Some(DolbySurroundExMode::SurroundExOrProLogicIIx)
    );
    assert_eq!(surround.dolby_surround_mode, None);
    let production = surround.production.unwrap();
    assert_eq!(production.peak_mixing_level_db_spl, 100);
    assert_eq!(production.room_type, RoomType::LargeRoomXCurve);
    assert_eq!(production.converter_type, AnalogDigitalConverterType::Hdcd);

    // Без опций метаданных ffmpeg не пишет informational/mixing блоки.
    let plain = first_info("stereo-32k-640k.eac3").bsi;
    assert_eq!((plain.informational, plain.mixing), (None, None));
    assert_eq!(plain.program.dialogue_normalization.code(), 31);
}

/// Уровни сведения: `ffprobe -show_frames` для 3f — Lo/Ro center −6 дБ,
/// Lt/Rt center −4,5 дБ; для 5.1 — Lo/Ro center 0 дБ, Lt/Rt center −1,5 дБ,
/// Lo/Ro surround −3 дБ (опции 0,841/0,5/1,0/0,707 из `generate.sh`).
#[test]
fn mixing_metadata_matches_encoder_and_ffprobe() {
    let three = first_info("3f-48k-256k.eac3").bsi.mixing.unwrap();
    assert_eq!(three.preferred_downmix, Some(PreferredStereoDownmix::LoRo));
    assert_eq!(three.lo_ro_center_mix_level.unwrap().gain(), 0.5);
    assert_eq!(three.lt_rt_center_mix_level.unwrap().gain(), 0.595);
    assert_eq!(three.lt_rt_surround_mix_level, None);
    assert_eq!(three.mixing_option, Some(MixingOption::Basic));

    let surround = first_info("5p1-48k-384k.eac3").bsi.mixing.unwrap();
    assert_eq!(
        surround.preferred_downmix,
        Some(PreferredStereoDownmix::LtRt)
    );
    assert_eq!(surround.lt_rt_center_mix_level.unwrap().gain(), 0.841);
    assert_eq!(surround.lt_rt_surround_mix_level.unwrap().gain(), 0.5);
    assert_eq!(surround.lo_ro_center_mix_level.unwrap().gain(), 1.0);
    assert_eq!(surround.lo_ro_surround_mix_level.unwrap().gain(), 0.707);
    assert_eq!(surround.lfe_mix_level_code, None);
    assert_eq!(surround.program_scale_factor, None);
}
