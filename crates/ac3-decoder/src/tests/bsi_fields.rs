//! BSI фикстур ffmpeg против известных настроек кодера (см. `generate.sh`) и
//! метаданных `ffprobe` (записаны в `tests/fixtures/README.md`).

use super::support::{FIXTURES, fixture, split_frames};
use crate::read_syncframe_info;
use crate::syntax::annex_d::{
    AnalogDigitalConverterType, BsiExtension, DolbyHeadphoneMode, DolbySurroundExMode,
    PreferredStereoDownmix,
};
use crate::syntax::bsi_values::{
    AudioCodingMode, CenterMixLevel, DolbySurroundMode, RoomType, ServiceType, SurroundMixLevel,
};
use crate::tables::frame_size::SampleRateCode;

/// Ожидаемые поля заголовка одной фикстуры.
struct Expected {
    name: &'static str,
    sample_rate_hz: u32,
    bitrate_kbps: u16,
    mode: AudioCodingMode,
    lfe_on: bool,
    dialnorm_db: i8,
    service: ServiceType,
    center: Option<CenterMixLevel>,
    surround: Option<SurroundMixLevel>,
}

const fn expected(
    name: &'static str,
    sample_rate_hz: u32,
    bitrate_kbps: u16,
    mode: AudioCodingMode,
    lfe_on: bool,
) -> Expected {
    // Значения по умолчанию кодера ffmpeg: dialnorm −31, main service,
    // cmixlev −4,5 дБ и surmixlev −6 дБ там, где поля существуют.
    let center = if matches!(
        mode,
        AudioCodingMode::ThreeFront
            | AudioCodingMode::ThreeFrontMonoSurround
            | AudioCodingMode::ThreeFrontStereoSurround
    ) {
        Some(CenterMixLevel::Minus4p5Db)
    } else {
        None
    };
    let surround = if mode.code() & 0x4 != 0 {
        Some(SurroundMixLevel::Minus6Db)
    } else {
        None
    };
    Expected {
        name,
        sample_rate_hz,
        bitrate_kbps,
        mode,
        lfe_on,
        dialnorm_db: -31,
        service: ServiceType::CompleteMain,
        center,
        surround,
    }
}

fn expectations() -> Vec<Expected> {
    use AudioCodingMode::*;
    vec![
        Expected {
            dialnorm_db: -27,
            service: ServiceType::Commentary,
            ..expected("mono-48k-96k.ac3", 48_000, 96, Mono, false)
        },
        Expected {
            dialnorm_db: -20,
            ..expected("stereo-48k-192k.ac3", 48_000, 192, Stereo, false)
        },
        expected("stereo-44k1-128k-plain.ac3", 44_100, 128, Stereo, false),
        expected("stereo-48k-32k.ac3", 48_000, 32, Stereo, false),
        Expected {
            center: Some(CenterMixLevel::Minus6Db),
            ..expected("3f-48k-256k.ac3", 48_000, 256, ThreeFront, false)
        },
        Expected {
            surround: Some(SurroundMixLevel::Minus3Db),
            ..expected("2f1r-32k-192k.ac3", 32_000, 192, StereoMonoSurround, false)
        },
        Expected {
            service: ServiceType::VisuallyImpaired,
            ..expected(
                "3f1r-48k-256k.ac3",
                48_000,
                256,
                ThreeFrontMonoSurround,
                false,
            )
        },
        expected(
            "2f2r-48k-320k.ac3",
            48_000,
            320,
            StereoStereoSurround,
            false,
        ),
        expected(
            "3f2r-48k-448k-cpl.ac3",
            48_000,
            448,
            ThreeFrontStereoSurround,
            false,
        ),
        expected("mono-lfe-48k-128k.ac3", 48_000, 128, Mono, true),
        expected("stereo-lfe-44k1-192k.ac3", 44_100, 192, Stereo, true),
        expected(
            "2f1r-lfe-48k-256k.ac3",
            48_000,
            256,
            StereoMonoSurround,
            true,
        ),
        expected("3f-lfe-32k-256k.ac3", 32_000, 256, ThreeFront, true),
        expected(
            "3f1r-lfe-48k-384k.ac3",
            48_000,
            384,
            ThreeFrontMonoSurround,
            true,
        ),
        expected(
            "5p1-48k-640k-annexd.ac3",
            48_000,
            640,
            ThreeFrontStereoSurround,
            true,
        ),
        expected(
            "5p1-44k1-448k-annexd.ac3",
            44_100,
            448,
            ThreeFrontStereoSurround,
            true,
        ),
        Expected {
            service: ServiceType::Karaoke,
            ..expected(
                "5p1-32k-192k.ac3",
                32_000,
                192,
                ThreeFrontStereoSurround,
                true,
            )
        },
        expected("stereo-48k-160k-cpl-high.ac3", 48_000, 160, Stereo, false),
    ]
}

#[test]
fn every_frame_of_every_fixture_has_the_encoded_bsi() {
    let expectations = expectations();
    assert_eq!(expectations.len(), FIXTURES.len());
    for expected in &expectations {
        for frame in split_frames(fixture(expected.name)) {
            let info = read_syncframe_info(frame).unwrap();
            let bsi = &info.bsi;
            let context = expected.name;
            assert_eq!(info.sample_rate_hz, expected.sample_rate_hz, "{context}");
            assert_eq!(info.bitrate_kbps, expected.bitrate_kbps, "{context}");
            assert_eq!(info.frame_length_bytes, frame.len(), "{context}");
            assert_eq!(info.samples_per_channel, 1536, "{context}");
            assert_eq!(bsi.audio_coding_mode, expected.mode, "{context}");
            assert_eq!(bsi.lfe_on, expected.lfe_on, "{context}");
            assert_eq!(
                bsi.program.dialogue_normalization.level_db(),
                expected.dialnorm_db,
                "{context}"
            );
            assert_eq!(bsi.service, expected.service, "{context}");
            assert_eq!(bsi.center_mix_level, expected.center, "{context}");
            assert_eq!(bsi.surround_mix_level, expected.surround, "{context}");
            assert_eq!(bsi.dual_mono_second_program, None, "{context}");
            assert!(bsi.additional_bsi.is_empty(), "{context}");
        }
    }
}

#[test]
fn optional_bsi_fields_follow_encoder_options() {
    let first = |name| read_syncframe_info(fixture(name)).unwrap().bsi;
    assert_eq!(
        first("stereo-48k-192k.ac3").dolby_surround_mode,
        Some(DolbySurroundMode::Encoded)
    );
    assert_eq!(
        first("stereo-44k1-128k-plain.ac3").dolby_surround_mode,
        Some(DolbySurroundMode::NotEncoded)
    );
    assert_eq!(
        first("stereo-48k-32k.ac3").dolby_surround_mode,
        Some(DolbySurroundMode::NotIndicated)
    );
    assert_eq!(first("mono-48k-96k.ac3").dolby_surround_mode, None);
    let three_front = first("3f-48k-256k.ac3");
    let production = three_front.program.production.unwrap();
    assert_eq!(production.peak_mixing_level_db_spl, 105);
    assert_eq!(production.room_type, RoomType::LargeRoomXCurve);
    let quad = first("2f2r-48k-320k.ac3");
    assert!(quad.copyright && !quad.original_bitstream);
    let stereo = first("stereo-48k-192k.ac3");
    assert!(!stereo.copyright && stereo.original_bitstream);
    assert_eq!(stereo.bitstream_id, 8);
    assert_eq!(
        stereo.extension,
        BsiExtension::TimeCode {
            first_half: None,
            second_half: None
        }
    );
    assert_eq!(
        read_syncframe_info(fixture("2f1r-32k-192k.ac3"))
            .unwrap()
            .sample_rate_code,
        SampleRateCode::Rate32000
    );
}

#[test]
fn annex_d_alternate_bsi_matches_encoder_options() {
    // -dmix_mode ltrt -ltrt_cmixlev 0.841 -ltrt_surmixlev 0.5 -loro_cmixlev 1.0
    // -loro_surmixlev 0.707 -dsurex_mode on -dheadphone_mode off -ad_conv_type hdcd
    let explicit = read_syncframe_info(fixture("5p1-48k-640k-annexd.ac3"))
        .unwrap()
        .bsi;
    assert_eq!(explicit.bitstream_id, 6);
    let BsiExtension::Alternate {
        downmix: Some(downmix),
        extra: Some(extra),
    } = explicit.extension
    else {
        panic!("Annex D fields expected, got {:?}", explicit.extension);
    };
    assert_eq!(downmix.preferred_downmix, PreferredStereoDownmix::LtRt);
    assert_eq!(downmix.lt_rt_center_mix_level.gain(), 0.841);
    assert_eq!(downmix.lt_rt_surround_mix_level.gain(), 0.5);
    // ffprobe: center_mix_level_db=0.0, surround_mix_level_db=-3.0 (уровни Lo/Ro).
    assert_eq!(downmix.lo_ro_center_mix_level.gain(), 1.0);
    assert_eq!(downmix.lo_ro_surround_mix_level.gain(), 0.707);
    assert_eq!(
        extra.dolby_surround_ex_mode,
        DolbySurroundExMode::SurroundExOrProLogicIIx
    );
    assert_eq!(extra.dolby_headphone_mode, DolbyHeadphoneMode::NotEncoded);
    assert_eq!(extra.converter_type, AnalogDigitalConverterType::Hdcd);
    assert_eq!(extra.reserved_bits, 0);

    // -dmix_mode loro -dsurex_mode dpliiz, уровни по умолчанию (ffprobe: −4,5 / −6 дБ).
    let defaults = read_syncframe_info(fixture("5p1-44k1-448k-annexd.ac3"))
        .unwrap()
        .bsi;
    let BsiExtension::Alternate {
        downmix: Some(downmix),
        extra: Some(extra),
    } = defaults.extension
    else {
        panic!("Annex D fields expected, got {:?}", defaults.extension);
    };
    assert_eq!(downmix.preferred_downmix, PreferredStereoDownmix::LoRo);
    assert_eq!(downmix.lo_ro_center_mix_level.gain(), 0.595);
    assert_eq!(downmix.lo_ro_surround_mix_level.gain(), 0.5);
    assert_eq!(
        extra.dolby_surround_ex_mode,
        DolbySurroundExMode::ProLogicIIz
    );
    assert_eq!(extra.dolby_headphone_mode, DolbyHeadphoneMode::NotIndicated);
    assert_eq!(extra.converter_type, AnalogDigitalConverterType::Standard);
}
