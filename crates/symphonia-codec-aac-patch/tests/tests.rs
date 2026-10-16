use symphonia_codec_aac::{AacDecoder, AdtsReader};
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_AAC,
};
use symphonia_core::errors;
use symphonia_core::formats::probe::ProbeableFormat;
use symphonia_core::io::MediaSourceStream;
use symphonia_core::packet::PacketRef;

fn test_decode(data: Vec<u8>) -> symphonia_core::errors::Result<()> {
    let data = std::io::Cursor::new(data);

    let mss = MediaSourceStream::new(Box::new(data), Default::default());

    let mut reader = AdtsReader::try_probe_new(mss, Default::default())?;

    let mut decoder = AacDecoder::try_new(
        AudioCodecParameters::new().for_codec(CODEC_ID_AAC),
        &AudioDecoderOptions::default(),
    )?;

    loop {
        match reader.next_packet()? {
            Some(packet) => {
                let _ = decoder.decode(&packet);
            }
            None => break,
        };
    }

    Ok(())
}

#[test]
fn invalid_channels_aac() {
    let file = vec![
        0xff, 0xf1, 0xaf, 0xce, 0x02, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xfb,
        0xaf,
    ];

    let err = test_decode(file).unwrap_err();

    assert!(matches!(err, errors::Error::Unsupported(_)));
}

// ---------------------------------------------------------------------------------------------
// AAC-LC с channel_configuration = 0 и Program Config Element (PCE), 5.1 от ffmpeg.
// ---------------------------------------------------------------------------------------------

/// ASC, который ffmpeg пишет в Matroska/MP4 для 5.1(side): channelConfiguration 0 + PCE
/// (front SCE, front CPE, back CPE, LFE), комментарий "Lavc63.1.102" и SBR-sync-хвост.
const FFMPEG_PCE_5P1_ASC: [u8; 25] = [
    0x11, 0x80, 0x04, 0xc8, 0x05, 0x00, 0x01, 0x08, 0x80, 0x0c, b'L', b'a', b'v', b'c', b'6', b'3',
    b'.', b'1', b'.', b'1', b'0', b'2', 0x56, 0xe5, 0x00,
];
/// 1 с синуса 880 Гц ТОЛЬКО в FC; каждый raw_data_block начинается с in-band PCE.
const PCE_5P1_ADTS: &[u8] = include_bytes!("fixtures/pce-5p1-fc-880hz.adts");
const TONE_HZ: f64 = 880.0;
const SAMPLE_RATE: u32 = 48_000;
/// Индексы planar-каналов Symphonia для AAC 5.1: FL, FR, FC, LFE, RL, RR.
const FRONT_CENTER_PLANE: usize = 2;

/// Режет ADTS-поток на raw_data_block-и (заголовок 7 байт: protection_absent = 1).
fn adts_raw_data_blocks(stream: &[u8]) -> Vec<&[u8]> {
    let mut blocks = Vec::new();
    let mut offset = 0;
    while offset + 7 <= stream.len() {
        assert_eq!(stream[offset], 0xff, "ADTS syncword at {offset}");
        let frame_length = ((usize::from(stream[offset + 3]) & 0x03) << 11)
            | (usize::from(stream[offset + 4]) << 3)
            | (usize::from(stream[offset + 5]) >> 5);
        assert!(frame_length > 7 && offset + frame_length <= stream.len());
        blocks.push(&stream[offset + 7..offset + frame_length]);
        offset += frame_length;
    }
    blocks
}

/// Декодирует все блоки и возвращает planar f32 по каналам вместе с итоговым spec.
fn decode_planar(decoder: &mut AacDecoder, blocks: &[&[u8]]) -> (Vec<Vec<f32>>, u32, usize) {
    use symphonia_core::units::{Duration, Timestamp};
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let (mut rate, mut channels) = (0, 0);
    for block in blocks {
        let packet = PacketRef::new(0, Timestamp::ZERO, Duration::ZERO, block);
        let decoded = decoder.decode_ref(&packet).expect("PCE-configured AAC frame must decode");
        rate = decoded.spec().rate();
        channels = decoded.spec().channels().count();
        let mut frame_planes: Vec<Vec<f32>> = Vec::new();
        decoded.copy_to_vecs_planar::<f32>(&mut frame_planes);
        planes.resize_with(frame_planes.len(), Vec::new);
        for (all, frame) in planes.iter_mut().zip(frame_planes) {
            all.extend(frame);
        }
    }
    (planes, rate, channels)
}

fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}

/// Доля энергии сигнала, приходящаяся на частоту `hz` (алгоритм Гёрцеля), 0..=1.
fn tone_energy_ratio(samples: &[f32], hz: f64) -> f64 {
    let coeff = 2.0 * (2.0 * std::f64::consts::PI * hz / f64::from(SAMPLE_RATE)).cos();
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for sample in samples {
        let s0 = f64::from(*sample) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    // Для чистого синуса амплитуды A мощность Гёрцеля = (N*A/2)^2, энергия = N*A^2/2:
    // нормировка 2*power/(N*energy) даёт 1.0 для чистого тона.
    let total_energy: f64 = samples.iter().map(|s| f64::from(*s).powi(2)).sum();
    (2.0 * power / (samples.len() as f64 * total_energy)).min(1.0)
}

fn pce_decoder() -> AacDecoder {
    let mut params = AudioCodecParameters::new();
    params.for_codec(CODEC_ID_AAC).with_extra_data(Box::new(FFMPEG_PCE_5P1_ASC));
    AacDecoder::try_new(&params, &AudioDecoderOptions::default())
        .expect("PCE-configured 5.1 AAC-LC must be accepted")
}

#[test]
fn pce_configured_5p1_decodes_tone_only_on_front_center() {
    let blocks = adts_raw_data_blocks(PCE_5P1_ADTS);
    assert!(blocks.len() >= 40, "fixture is ~1 s of AAC: {} blocks", blocks.len());

    let (planes, rate, channels) = decode_planar(&mut pce_decoder(), &blocks);

    assert_eq!((rate, channels, planes.len()), (SAMPLE_RATE, 6, 6));
    let center = &planes[FRONT_CENTER_PLANE];
    assert!(rms(center) > 0.05, "FC must carry the tone, rms={}", rms(center));
    assert!(
        tone_energy_ratio(center, TONE_HZ) > 0.9,
        "FC energy must be at 880 Hz, ratio={}",
        tone_energy_ratio(center, TONE_HZ)
    );
    for (index, plane) in planes.iter().enumerate().filter(|(i, _)| *i != FRONT_CENTER_PLANE) {
        assert!(rms(plane) < 0.005, "plane {index} must be silent, rms={}", rms(plane));
    }
}

#[test]
fn pce_extra_data_truncated_inside_pce_is_rejected_without_panic() {
    for length in 2..22 {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_extra_data(FFMPEG_PCE_5P1_ASC[..length].to_vec().into_boxed_slice());
        assert!(
            AacDecoder::try_new(&params, &AudioDecoderOptions::default()).is_err(),
            "prefix of {length} bytes must be rejected"
        );
    }
}

#[test]
fn pce_with_unmatched_layout_is_unsupported() {
    // AOT 2, 48 кГц, config 0, PCE: front CPE + back CPE (квадро, индексного эквивалента нет).
    let asc = [0x11, 0x80, 0x04, 0xc4, 0x04, 0x00, 0x21, 0x10, 0x00];
    let mut params = AudioCodecParameters::new();
    params.for_codec(CODEC_ID_AAC).with_extra_data(asc.to_vec().into_boxed_slice());

    let error = AacDecoder::try_new(&params, &AudioDecoderOptions::default()).err();

    assert!(matches!(error, Some(errors::Error::Unsupported(_))), "{error:?}");
}
