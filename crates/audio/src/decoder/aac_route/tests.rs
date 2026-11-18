//! Маршрут AAC на настоящих кадрах: какой декодер берёт поток и что слышно на выходе.

use std::path::{Path, PathBuf};

use audio_core::{AudioDecoderConfig, AudioDecoderHandle, EncodedAudioPacket};

use super::super::{SymphoniaAudioDecoder, create_audio_decoder};

const TRACK_ID: u32 = 1;
const AAC_CODEC_ID: &str = "A_AAC";
/// Паспорт HE-AAC v1 стерео, как у Instagram (ядро 22,05 кГц → 44,1 кГц).
const INSTAGRAM_V1_ASC: [u8; 4] = [0x2b, 0x92, 0x08, 0x00];
/// Паспорт xHE-AAC (USAC, тип 42): FFmpeg 8.1 его умеет, но маршрут туда не ведёт.
const USAC_ASC: [u8; 3] = [0xf9, 0x46, 0x40];
/// AAC-LC 5.1 с PCE от ffmpeg (фикстура патча Symphonia).
const FFMPEG_PCE_5P1_ASC: [u8; 25] = [
    0x11, 0x80, 0x04, 0xc8, 0x05, 0x00, 0x01, 0x08, 0x80, 0x0c, b'L', b'a', b'v', b'c', b'6', b'3',
    b'.', b'1', b'.', b'1', b'0', b'2', 0x56, 0xe5, 0x00,
];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .to_path_buf()
}

fn he_aac_fixture(file_name: &str) -> PathBuf {
    crates_dir()
        .join("audio-ffmpeg/tests/fixtures/he-aac")
        .join(file_name)
}

/// Кадры ADTS без заголовков — как их отдаёт демуксер MPEG-TS.
fn adts_frames(path: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).expect("fixture reads");
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset + 7 <= bytes.len() {
        let protection_absent = bytes[offset + 1] & 1 == 1;
        let frame_length = (usize::from(bytes[offset + 3] & 3) << 11)
            | (usize::from(bytes[offset + 4]) << 3)
            | (usize::from(bytes[offset + 5]) >> 5);
        let header_length = if protection_absent { 7 } else { 9 };
        frames.push(bytes[offset + header_length..offset + frame_length].to_vec());
        offset += frame_length;
    }
    frames
}

/// Конфигурация дорожки: частота и каналы — как их сообщил бы контейнер.
fn track_config(
    container_sample_rate: u32,
    container_channels: u32,
    audio_specific_config: Option<&[u8]>,
) -> AudioDecoderConfig {
    AudioDecoderConfig::from_track_metadata(
        TRACK_ID,
        AAC_CODEC_ID.to_owned(),
        Some(container_sample_rate),
        Some(container_channels),
    )
    .with_codec_private(audio_specific_config.map(<[u8]>::to_vec))
}

/// Декодирует все кадры; возвращает PCM и частоту каждого непустого буфера.
fn decode_frames(decoder: &mut AudioDecoderHandle, frames: &[Vec<u8>]) -> (Vec<f32>, Vec<u32>) {
    let mut samples = Vec::new();
    let mut rates = Vec::new();
    for frame in frames {
        let pcm = decoder
            .decode(&EncodedAudioPacket::without_timing(TRACK_ID, frame))
            .expect("frame decodes");
        if !pcm.is_empty() {
            rates.push(
                decoder
                    .decoded_output_spec()
                    .expect("spec after PCM")
                    .sample_rate,
            );
            samples.extend(pcm);
        }
    }
    (samples, rates)
}

#[test]
fn plain_low_complexity_pcm_is_bit_identical_to_direct_symphonia() {
    // Регресс AAC-LC: маршрут не должен менять ни одного отсчёта.
    let pce_adts =
        crates_dir().join("symphonia-codec-aac-patch/tests/fixtures/pce-5p1-fc-880hz.adts");
    let frames = adts_frames(&pce_adts);
    let config = track_config(48_000, 6, Some(&FFMPEG_PCE_5P1_ASC));

    let mut routed = create_audio_decoder(config.clone()).expect("routed decoder");
    let mut direct: AudioDecoderHandle =
        Box::new(SymphoniaAudioDecoder::new(&config).expect("direct Symphonia decoder"));
    let (routed_pcm, routed_rates) = decode_frames(&mut routed, &frames);
    let (direct_pcm, _) = decode_frames(&mut direct, &frames);

    assert!(!routed_pcm.is_empty());
    assert_eq!(routed_pcm, direct_pcm);
    assert!(routed_rates.iter().all(|rate| *rate == 48_000));
}

#[test]
fn xhe_aac_is_not_routed_to_ffmpeg() {
    // Symphonia честно отказывает в xHE-AAC тем же типизированным отказом, что и раньше.
    let result = create_audio_decoder(track_config(48_000, 2, Some(&USAC_ASC)));
    let error = result.err().expect("xHE-AAC stays unsupported");
    assert_eq!(
        error.downcast_ref::<audio_core::AudioDecoderError>(),
        Some(&audio_core::AudioDecoderError::UnsupportedCodec {
            codec_id: AAC_CODEC_ID.to_owned()
        })
    );
    assert!(
        error
            .downcast_ref::<audio_ffmpeg::AudioFfmpegError>()
            .is_none()
    );
}

#[cfg(feature = "ffmpeg")]
mod with_ffmpeg {
    use super::*;

    #[test]
    fn site_passport_goes_straight_to_he_aac_at_the_output_rate() {
        // Тот же поток, но паспорт иерархический, как у Instagram.
        let frames = adts_frames(&he_aac_fixture("v1-stereo-implicit.adts"));
        let mut decoder = create_audio_decoder(track_config(22_050, 2, Some(&INSTAGRAM_V1_ASC)))
            .expect("decoder");
        let (pcm, rates) = decode_frames(&mut decoder, &frames);

        assert!(!rates.is_empty());
        assert!(rates.iter().all(|rate| *rate == 44_100), "rates: {rates:?}");
        assert_eq!(decoder.channels(), 2);
        assert!(pcm.len() > 44_100 * 2, "about 2 s of stereo PCM");
    }

    #[test]
    fn implicit_he_aac_switches_on_the_first_packet_without_a_muffled_buffer() {
        // ADTS в MPEG-TS: паспорта нет, контейнер знает ядро 22,05 кГц.
        let frames = adts_frames(&he_aac_fixture("v1-stereo-implicit.adts"));
        let mut decoder = create_audio_decoder(track_config(22_050, 2, None)).expect("decoder");

        let first = decoder
            .decode(&EncodedAudioPacket::without_timing(TRACK_ID, &frames[0]))
            .expect("first packet decodes");
        assert!(!first.is_empty(), "the very first packet already plays");
        assert_eq!(
            decoder.decoded_output_spec().map(|spec| spec.sample_rate),
            Some(44_100)
        );

        let (_, rates) = decode_frames(&mut decoder, &frames[1..]);
        // Ни одного буфера на частоте ядра: player-core не пересобирает вывод.
        assert!(rates.iter().all(|rate| *rate == 44_100), "rates: {rates:?}");
    }

    #[test]
    fn low_complexity_passport_with_sbr_inside_packets_switches_too() {
        // Радио, перепакованное ffmpeg в MP4/fMP4: паспорт честно говорит «AAC-LC 22,05 кГц»
        // (2 байта из заголовка ADTS), а SBR лежит только внутри кадров.
        let low_complexity_22k_stereo = [0x13, 0x90];
        let frames = adts_frames(&he_aac_fixture("v1-stereo-implicit.adts"));
        let mut decoder =
            create_audio_decoder(track_config(22_050, 2, Some(&low_complexity_22k_stereo)))
                .expect("decoder");
        let (_, rates) = decode_frames(&mut decoder, &frames);
        assert!(!rates.is_empty());
        assert!(rates.iter().all(|rate| *rate == 44_100), "rates: {rates:?}");
    }

    #[test]
    fn implicit_parametric_stereo_with_a_mono_header_plays_as_stereo() {
        // ADTS v2: заголовок говорит «1 канал», стерео даёт PS.
        let frames = adts_frames(&he_aac_fixture("v2-ps-implicit.adts"));
        let mut decoder = create_audio_decoder(track_config(22_050, 1, None)).expect("decoder");
        let (_, rates) = decode_frames(&mut decoder, &frames);
        assert!(rates.iter().all(|rate| *rate == 44_100));
        assert_eq!(decoder.channels(), 2);
    }

    #[test]
    fn decoder_choice_survives_reset_after_seek() {
        let frames = adts_frames(&he_aac_fixture("v1-stereo-implicit.adts"));
        let mut decoder = create_audio_decoder(track_config(22_050, 2, None)).expect("decoder");
        decode_frames(&mut decoder, &frames[..3]);
        decoder.reset().expect("reset");
        let (_, rates) = decode_frames(&mut decoder, &frames[20..]);
        assert!(!rates.is_empty());
        assert!(
            rates.iter().all(|rate| *rate == 44_100),
            "rates after reset: {rates:?}"
        );
    }

    #[test]
    fn corrupt_packet_on_he_aac_is_skipped_and_playback_continues() {
        let frames = adts_frames(&he_aac_fixture("v1-stereo-implicit.adts"));
        let mut decoder = create_audio_decoder(track_config(22_050, 2, Some(&INSTAGRAM_V1_ASC)))
            .expect("decoder");
        decode_frames(&mut decoder, &frames[..3]);
        let garbage = [0xFF_u8; 300];
        let skipped = decoder
            .decode(&EncodedAudioPacket::without_timing(TRACK_ID, &garbage))
            .expect("corrupt packet is not fatal");
        assert!(skipped.is_empty());
        let (pcm, _) = decode_frames(&mut decoder, &frames[3..]);
        assert!(!pcm.is_empty());
    }

    #[test]
    fn packet_of_another_track_is_rejected() {
        let frames = adts_frames(&he_aac_fixture("v1-stereo-implicit.adts"));
        let mut decoder = create_audio_decoder(track_config(22_050, 2, Some(&INSTAGRAM_V1_ASC)))
            .expect("decoder");
        let foreign = EncodedAudioPacket::without_timing(TRACK_ID + 1, &frames[0]);
        assert!(decoder.decode(&foreign).is_err());
    }
}

#[cfg(not(feature = "ffmpeg"))]
mod without_ffmpeg {
    use super::super::InBandSbrDetected;
    use super::*;

    #[test]
    fn explicit_he_aac_is_refused_honestly() {
        let error = create_audio_decoder(track_config(22_050, 2, Some(&INSTAGRAM_V1_ASC)))
            .err()
            .expect("HE-AAC needs FFmpeg");
        assert_eq!(
            error.downcast_ref::<audio_ffmpeg::AudioFfmpegError>(),
            Some(&audio_ffmpeg::AudioFfmpegError::FeatureDisabled)
        );
    }

    #[test]
    fn implicit_he_aac_reports_why_it_cannot_switch() {
        let frames = adts_frames(&he_aac_fixture("v1-stereo-implicit.adts"));
        let mut decoder = create_audio_decoder(track_config(22_050, 2, None)).expect("decoder");
        let error = decoder
            .decode(&EncodedAudioPacket::without_timing(TRACK_ID, &frames[0]))
            .expect_err("no muffled core without FFmpeg");
        assert_eq!(
            error.downcast_ref::<audio_ffmpeg::AudioFfmpegError>(),
            Some(&audio_ffmpeg::AudioFfmpegError::FeatureDisabled)
        );
        assert!(format!("{error:#}").contains("implicitly signalled HE-AAC"));
    }

    #[test]
    fn in_band_sbr_signal_is_a_typed_error() {
        // Сигнал маршрута не теряет тип при прохождении через anyhow.
        let error = anyhow::Error::new(InBandSbrDetected {
            codec_id: AAC_CODEC_ID.to_owned(),
        });
        assert!(error.downcast_ref::<InBandSbrDetected>().is_some());
    }
}
