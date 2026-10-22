//! Функциональные тесты float-декодирования Opus (cpu-parity 06d).
//!
//! Что закрепляют:
//! - PCM приходит с float-точностью libopus, а не квантованным в 16 бит;
//! - результат совпадает с эталонным `opus_decode_float` той же libopus;
//! - OpusHead output gain применяется ровно один раз;
//! - пики выше 0 dBFS ограничиваются так же, как это делал i16 API libopus
//!   (soft clip), включая multistream и память между packet-ами;
//! - reset забывает память soft clip, как свежий decoder;
//! - битый packet по-прежнему даёт пустой PCM, а decoder продолжает работать.

use anyhow::{Context, Result, bail};
use opus::{Application, Channels, Decoder, Encoder, MSDecoder, MSEncoder, SoftClip};

use super::header::{OpusBackendConfig, parse_opus_head};
use super::tests::{ACCEPTANCE_5_1_CODEC_MAPPING, ACCEPTANCE_5_1_OPUS_HEAD};
use super::{AudioDecoderConfig, EncodedAudioPacket};
use crate::decoder::create_audio_decoder;

/// Частота Opus playback.
const SAMPLE_RATE: u32 = 48_000;

/// Frames в одном 20 ms packet-е.
const FRAMES_PER_PACKET: usize = 960;

/// Достаточно packet-ов, чтобы пройти разгон encoder-а и границы packet-ов.
const PACKET_COUNT: usize = 12;

/// Максимальный размер закодированного packet-а для encoder-а.
const MAX_ENCODED_PACKET_BYTES: usize = 4_000;

/// Буфер эталонного decoder-а: 120 ms на канал, как у production adapter-а.
const REFERENCE_MAX_FRAMES: usize = 5_760;

/// Шаг 16-битной сетки, на которую прежний i16-путь квантовал PCM.
const I16_FULL_SCALE: f32 = 32_768.0;

/// Track ID, под которым тесты создают decoder.
const TRACK_ID: u32 = 7;

/// Синус с разной фазой на каждом канале, interleaved, для `packet_index`.
fn sine_packet(amplitude: f32, channel_count: usize, packet_index: usize) -> Vec<f32> {
    let mut interleaved = Vec::with_capacity(FRAMES_PER_PACKET * channel_count);
    for frame_index in 0..FRAMES_PER_PACKET {
        let absolute_frame = packet_index * FRAMES_PER_PACKET + frame_index;
        let time_seconds = absolute_frame as f32 / SAMPLE_RATE as f32;
        for channel_index in 0..channel_count {
            let phase = std::f32::consts::TAU * 440.0 * time_seconds + channel_index as f32 * 0.7;
            interleaved.push(amplitude * phase.sin());
        }
    }
    interleaved
}

/// Кодирует stereo-синус настоящим libopus encoder-ом.
fn encode_stereo_packets(amplitude: f32) -> Result<Vec<Vec<u8>>> {
    let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Audio)?;
    (0..PACKET_COUNT)
        .map(|packet_index| {
            let source = sine_packet(amplitude, 2, packet_index);
            Ok(encoder.encode_vec_float(&source, MAX_ENCODED_PACKET_BYTES)?)
        })
        .collect()
}

/// Кодирует 5.1-синус multistream encoder-ом с mapping acceptance asset-а.
fn encode_5_1_packets(amplitude: f32) -> Result<Vec<Vec<u8>>> {
    let mut encoder = MSEncoder::new(
        SAMPLE_RATE,
        4,
        2,
        &ACCEPTANCE_5_1_CODEC_MAPPING,
        Application::Audio,
    )?;
    (0..PACKET_COUNT)
        .map(|packet_index| {
            let source = sine_packet(amplitude, 6, packet_index);
            Ok(encoder.encode_vec_float(&source, MAX_ENCODED_PACKET_BYTES)?)
        })
        .collect()
}

/// Family-0 stereo `OpusHead` с заданным output gain в Q7.8 dB.
fn family_zero_stereo_opus_head(output_gain_q8_db: i16) -> Vec<u8> {
    let mut opus_head = b"OpusHead".to_vec();
    opus_head.push(1); // version
    opus_head.push(2); // channel count
    opus_head.extend_from_slice(&312_u16.to_le_bytes()); // pre-skip
    opus_head.extend_from_slice(&SAMPLE_RATE.to_le_bytes()); // input sample rate
    opus_head.extend_from_slice(&output_gain_q8_db.to_le_bytes());
    opus_head.push(0); // mapping family 0
    opus_head
}

/// Production decoder через public factory.
fn production_decoder(
    channels: u32,
    codec_private: Option<Vec<u8>>,
) -> Result<audio_core::AudioDecoderHandle> {
    create_audio_decoder(
        AudioDecoderConfig::new(TRACK_ID, "A_OPUS", SAMPLE_RATE, channels)
            .with_codec_private(codec_private),
    )
}

/// Декодирует все packet-ы production decoder-ом.
fn decode_with_production(
    decoder: &mut audio_core::AudioDecoderHandle,
    packets: &[Vec<u8>],
) -> Result<Vec<Vec<f32>>> {
    packets
        .iter()
        .map(|packet| decoder.decode(&EncodedAudioPacket::without_timing(TRACK_ID, packet)))
        .collect()
}

/// Эталон libopus без нашего adapter-а: float или i16 API, single/multistream.
enum ReferenceDecoder {
    SingleStream(Decoder),
    Multistream(MSDecoder),
}

impl ReferenceDecoder {
    /// Эталонный stereo single-stream decoder.
    fn stereo() -> Result<Self> {
        Ok(Self::SingleStream(Decoder::new(
            SAMPLE_RATE,
            Channels::Stereo,
        )?))
    }

    /// Эталонный 5.1 multistream decoder с той же canonical mapping, что у production.
    fn surround_5_1() -> Result<Self> {
        let parsed_head = parse_opus_head(&ACCEPTANCE_5_1_OPUS_HEAD)?;
        let OpusBackendConfig::Multistream {
            streams,
            coupled_streams,
            canonical_mapping,
        } = parsed_head.backend
        else {
            bail!("acceptance OpusHead must describe a multistream backend");
        };
        Ok(Self::Multistream(MSDecoder::new(
            SAMPLE_RATE,
            streams,
            coupled_streams,
            &canonical_mapping,
        )?))
    }

    /// `opus_decode_float`: без soft clip и без квантизации.
    fn decode_float(&mut self, packet: &[u8], channel_count: usize) -> Result<Vec<f32>> {
        let mut output = vec![0.0_f32; REFERENCE_MAX_FRAMES * channel_count];
        let frame_count = match self {
            Self::SingleStream(decoder) => decoder.decode_float(packet, &mut output, false)?,
            Self::Multistream(decoder) => decoder.decode_float(packet, &mut output, false)?,
        };
        output.truncate(frame_count * channel_count);
        Ok(output)
    }

    /// `opus_decode` (i16 API): libopus сам делает soft clip и квантует в 16 бит.
    fn decode_i16(&mut self, packet: &[u8], channel_count: usize) -> Result<Vec<i16>> {
        let mut output = vec![0_i16; REFERENCE_MAX_FRAMES * channel_count];
        let frame_count = match self {
            Self::SingleStream(decoder) => decoder.decode(packet, &mut output, false)?,
            Self::Multistream(decoder) => decoder.decode(packet, &mut output, false)?,
        };
        output.truncate(frame_count * channel_count);
        Ok(output)
    }
}

/// Лежит ли сэмпл точно на 16-битной сетке прежнего i16-пути.
fn is_on_i16_grid(sample: f32) -> bool {
    (sample * I16_FULL_SCALE).fract() == 0.0
}

/// Проверяет float-точность и совпадение с эталонным float decode.
fn assert_float_precision_matches_reference(
    production_packets: &[Vec<f32>],
    reference_packets: &[Vec<f32>],
) -> Result<()> {
    let mut audible_samples = 0_usize;
    let mut off_grid_samples = 0_usize;
    for (packet_index, (production, reference)) in
        production_packets.iter().zip(reference_packets).enumerate()
    {
        if production.len() != reference.len() {
            bail!(
                "packet {packet_index}: {} samples, reference {}",
                production.len(),
                reference.len()
            );
        }
        for (sample_index, (produced, expected)) in production.iter().zip(reference).enumerate() {
            if (produced - expected).abs() > 1.0e-7 {
                bail!("packet {packet_index} sample {sample_index}: {produced} != {expected}");
            }
            if produced.abs() > 1.0e-3 {
                audible_samples += 1;
                if !is_on_i16_grid(*produced) {
                    off_grid_samples += 1;
                }
            }
        }
    }

    // Прежний путь клал каждый сэмпл ровно на сетку 1/32768; float-путь — почти никогда.
    assert!(audible_samples > 1_000, "signal must be audible");
    assert!(
        off_grid_samples * 2 > audible_samples,
        "PCM looks 16-bit quantised: {off_grid_samples} of {audible_samples} samples off grid"
    );
    Ok(())
}

/// Stereo PCM должен иметь float-точность libopus и совпадать с `opus_decode_float`.
#[test]
fn single_stream_pcm_has_float_precision_of_libopus() -> Result<()> {
    let packets = encode_stereo_packets(0.3)?;
    let mut decoder = production_decoder(2, None)?;
    let mut reference = ReferenceDecoder::stereo()?;

    let production_packets = decode_with_production(&mut decoder, &packets)?;
    let reference_packets = packets
        .iter()
        .map(|packet| reference.decode_float(packet, 2))
        .collect::<Result<Vec<_>>>()?;

    assert!(
        reference_packets
            .iter()
            .flatten()
            .all(|sample| sample.abs() <= 1.0),
        "moderate signal must stay below full scale, so soft clip is inactive"
    );
    assert_float_precision_matches_reference(&production_packets, &reference_packets)
}

/// 5.1 multistream PCM тоже идёт во float-точности и в canonical порядке каналов.
#[test]
fn multistream_pcm_has_float_precision_of_libopus() -> Result<()> {
    let packets = encode_5_1_packets(0.25)?;
    let mut decoder = production_decoder(6, Some(ACCEPTANCE_5_1_OPUS_HEAD.to_vec()))?;
    let mut reference = ReferenceDecoder::surround_5_1()?;

    let production_packets = decode_with_production(&mut decoder, &packets)?;
    let reference_packets = packets
        .iter()
        .map(|packet| reference.decode_float(packet, 6))
        .collect::<Result<Vec<_>>>()?;

    assert!(
        reference_packets
            .iter()
            .flatten()
            .all(|sample| sample.abs() <= 1.0),
        "moderate signal must stay below full scale, so soft clip is inactive"
    );
    assert_float_precision_matches_reference(&production_packets, &reference_packets)
}

/// OpusHead gain умножает float PCM ровно один раз.
#[test]
fn opus_head_gain_is_applied_exactly_once_to_float_pcm() -> Result<()> {
    let packets = encode_stereo_packets(0.3)?;
    let gain_q8_db = 6 * 256;
    let expected_gain = super::opus_output_gain_multiplier(gain_q8_db);
    let mut boosted_decoder =
        production_decoder(2, Some(family_zero_stereo_opus_head(gain_q8_db)))?;
    let mut reference = ReferenceDecoder::stereo()?;

    let boosted_packets = decode_with_production(&mut boosted_decoder, &packets)?;
    let mut checked_samples = 0_usize;
    for (boosted, packet) in boosted_packets.iter().zip(&packets) {
        let reference_samples = reference.decode_float(packet, 2)?;
        assert_eq!(boosted.len(), reference_samples.len());
        for (boosted_sample, reference_sample) in boosted.iter().zip(&reference_samples) {
            let expected = reference_sample * expected_gain;
            assert!(
                (boosted_sample - expected).abs() <= 1.0e-6,
                "{boosted_sample} != {expected}"
            );
            checked_samples += 1;
        }
    }

    // Gain +6 dB поднимает пики 0.3 примерно до 0.6: результат выше full scale
    // ещё не начинается, значит soft clip не вмешивается в проверку gain.
    assert!(checked_samples > 0);
    assert!(
        boosted_packets
            .iter()
            .flatten()
            .any(|sample| sample.abs() > 0.45),
        "gain must be visible in the output"
    );
    Ok(())
}

/// Проверяет, что production PCM совпадает с i16-путём libopus (soft clip внутри
/// libopus) с точностью до одного шага 16-битной квантизации.
fn assert_matches_libopus_i16_soft_clip(
    production_packets: &[Vec<f32>],
    reference_i16_packets: &[Vec<i16>],
) -> Result<()> {
    for (packet_index, (production, reference)) in production_packets
        .iter()
        .zip(reference_i16_packets)
        .enumerate()
    {
        if production.len() != reference.len() {
            bail!("packet {packet_index}: length mismatch");
        }
        for (sample_index, (produced, quantised)) in production.iter().zip(reference).enumerate() {
            if !(-1.0..=1.0).contains(produced) {
                bail!("packet {packet_index} sample {sample_index}: {produced} exceeds full scale");
            }
            let difference = (produced * I16_FULL_SCALE - f32::from(*quantised)).abs();
            if difference > 1.0 {
                bail!("packet {packet_index} sample {sample_index}: {produced} vs i16 {quantised}");
            }
        }
    }
    Ok(())
}

/// Сигнал выше 0 dBFS ограничивается так же, как в i16 API libopus.
#[test]
fn over_full_scale_stereo_is_soft_clipped_like_libopus_i16_api() -> Result<()> {
    let packets = encode_stereo_packets(1.6)?;
    let mut decoder = production_decoder(2, None)?;
    let mut float_reference = ReferenceDecoder::stereo()?;
    let mut i16_reference = ReferenceDecoder::stereo()?;
    let mut reference_clipper = SoftClip::new(Channels::Stereo);

    let production_packets = decode_with_production(&mut decoder, &packets)?;
    let mut raw_exceeds_full_scale = false;
    let mut reference_i16_packets = Vec::with_capacity(packets.len());
    for (production, packet) in production_packets.iter().zip(&packets) {
        let mut clipped_reference = float_reference.decode_float(packet, 2)?;
        raw_exceeds_full_scale |= clipped_reference.iter().any(|sample| sample.abs() > 1.0);
        reference_clipper.apply(&mut clipped_reference);
        // Тот же libopus soft clip поверх того же float decode: совпадение побитовое.
        assert_eq!(production, &clipped_reference);
        reference_i16_packets.push(i16_reference.decode_i16(packet, 2)?);
    }

    assert!(
        raw_exceeds_full_scale,
        "float decode must exceed full scale"
    );
    assert_matches_libopus_i16_soft_clip(&production_packets, &reference_i16_packets)
}

/// 5.1 multistream: per-channel soft clip равен per-stream soft clip libopus.
#[test]
fn over_full_scale_multistream_is_soft_clipped_like_libopus_i16_api() -> Result<()> {
    let packets = encode_5_1_packets(1.6)?;
    let mut decoder = production_decoder(6, Some(ACCEPTANCE_5_1_OPUS_HEAD.to_vec()))?;
    let mut float_reference = ReferenceDecoder::surround_5_1()?;
    let mut i16_reference = ReferenceDecoder::surround_5_1()?;

    let production_packets = decode_with_production(&mut decoder, &packets)?;
    let mut raw_exceeds_full_scale = false;
    let mut reference_i16_packets = Vec::with_capacity(packets.len());
    for packet in &packets {
        let raw_reference = float_reference.decode_float(packet, 6)?;
        raw_exceeds_full_scale |= raw_reference.iter().any(|sample| sample.abs() > 1.0);
        reference_i16_packets.push(i16_reference.decode_i16(packet, 6)?);
    }

    assert!(
        raw_exceeds_full_scale,
        "float decode must exceed full scale"
    );
    assert_matches_libopus_i16_soft_clip(&production_packets, &reference_i16_packets)
}

/// После reset decoder ведёт себя как свежий: ни codec state, ни память soft clip
/// прошлого сигнала не влияют на следующий packet.
#[test]
fn reset_forgets_soft_clip_memory_like_fresh_decoder() -> Result<()> {
    let packets = encode_stereo_packets(1.6)?;
    let (resumed_packet, history_packets) = packets
        .split_last()
        .context("test stream must contain packets")?;

    let mut fresh_decoder = production_decoder(2, None)?;
    let fresh_output = fresh_decoder.decode(&EncodedAudioPacket::without_timing(
        TRACK_ID,
        resumed_packet,
    ))?;

    let mut reset_decoder = production_decoder(2, None)?;
    decode_with_production(&mut reset_decoder, history_packets)?;
    reset_decoder.reset()?;
    let reset_output = reset_decoder.decode(&EncodedAudioPacket::without_timing(
        TRACK_ID,
        resumed_packet,
    ))?;

    assert_eq!(reset_output, fresh_output);

    // Предусловие теста: память soft clip после истории действительно меняет
    // этот packet, иначе тест не отличил бы забытый reset памяти.
    let mut raw_reference = ReferenceDecoder::stereo()?;
    let mut remembering_clipper = SoftClip::new(Channels::Stereo);
    for history_packet in history_packets {
        let mut history_pcm = raw_reference.decode_float(history_packet, 2)?;
        remembering_clipper.apply(&mut history_pcm);
    }
    let mut fresh_raw_reference = ReferenceDecoder::stereo()?;
    let mut resumed_with_memory = fresh_raw_reference.decode_float(resumed_packet, 2)?;
    remembering_clipper.apply(&mut resumed_with_memory);
    assert_ne!(
        resumed_with_memory, fresh_output,
        "soft clip memory of the history must matter for the resumed packet"
    );
    Ok(())
}

/// Битый packet пропускается как пустой PCM, decoder продолжает работать.
#[test]
fn corrupted_packet_yields_empty_pcm_and_decoder_keeps_working() -> Result<()> {
    let packets = encode_stereo_packets(0.3)?;
    let mut decoder = production_decoder(2, None)?;
    // TOC code 3 (несколько frames) без обязательного байта числа frames —
    // libopus отвечает OPUS_INVALID_PACKET.
    let corrupted_packet = [0xff_u8];

    let corrupted_output = decoder.decode(&EncodedAudioPacket::without_timing(
        TRACK_ID,
        &corrupted_packet,
    ))?;
    let next_output = decoder.decode(&EncodedAudioPacket::without_timing(TRACK_ID, &packets[0]))?;

    assert!(corrupted_output.is_empty());
    assert_eq!(next_output.len(), FRAMES_PER_PACKET * 2);
    Ok(())
}
