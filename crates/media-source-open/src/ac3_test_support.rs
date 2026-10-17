//! Общие помощники сквозных AC-3 / E-AC-3 тестов crate-а: сбор пакетов дорожки из
//! любого production demuxer-а, декод production `audio::create_audio_decoder` и
//! проверка «в каждой lane звучит свой тон» (Гёрцель по позиции канала).
//!
//! Используют: `local_media::ac3_elementary_tests` (локальный файл / TS),
//! `local_media::ts_concatenation_tests` (склеенные TS-записи),
//! `direct_progressive_open::ac3_elementary_tests` (прямой HTTP),
//! `native_startup::{dash,hls}::*ac3*` (адаптивные fMP4 вертикали).

use std::f32::consts::PI;
use std::thread;
use std::time::{Duration, Instant};

use audio::{AudioChannelPosition, AudioDecoderConfig, EncodedAudioPacket, create_audio_decoder};
use media_core::{DemuxReadEvent, Demuxer, Packet, TrackInfo, TrackKind};

/// AC-3 stereo 48 кГц: FL 440 Гц, FR 1000 Гц; 16 кадров (`audio/tests/fixtures/ac3`).
pub(crate) const STEREO_AC3: &[u8] =
    include_bytes!("../../audio/tests/fixtures/ac3/tones-stereo.ac3");
/// E-AC-3 5.1(side) 48 кГц: тон на канал (см. `SURROUND_TONES`); 16 пакетов.
pub(crate) const SURROUND_EAC3: &[u8] =
    include_bytes!("../../audio/tests/fixtures/ac3/tones-5.1.eac3");

/// Тоны stereo-фикстуры по позициям.
pub(crate) const STEREO_TONES: [(AudioChannelPosition, f32); 2] = [
    (AudioChannelPosition::FrontLeft, 440.0),
    (AudioChannelPosition::FrontRight, 1_000.0),
];
/// Тоны всех 5.1(side) фикстур (raw, MKV/MP4/fMP4) по позициям.
pub(crate) const SURROUND_TONES: [(AudioChannelPosition, f32); 6] = [
    (AudioChannelPosition::FrontLeft, 300.0),
    (AudioChannelPosition::FrontRight, 700.0),
    (AudioChannelPosition::FrontCenter, 500.0),
    (AudioChannelPosition::LowFrequencyEffects, 60.0),
    (AudioChannelPosition::SideLeft, 900.0),
    (AudioChannelPosition::SideRight, 1_100.0),
];
/// Samples на канал в одном AC-3 / E-AC-3 access unit (6 блоков × 256).
pub(crate) const SAMPLES_PER_ACCESS_UNIT: usize = 1_536;

/// Верхняя граница ожидания фонового demux/HTTP: регрессия не должна вешать тест.
const DEMUX_DEADLINE: Duration = Duration::from_secs(10);
/// Пауза между опросами, пока фоновый demuxer временно не готов.
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(2);
/// Минимальная нормированная энергия «своего» тона: тишина дала бы ~0.
const MINIMUM_OWN_TONE_ENERGY: f32 = 1e-4;
/// Свой тон сильнее любого чужого минимум в 100 раз (20 дБ).
const FOREIGN_TONE_REJECTION: f32 = 100.0;

/// Результат production-декода: interleaved PCM + настоящий формат последнего кадра.
pub(crate) struct DecodedPcm {
    pub(crate) interleaved: Vec<f32>,
    pub(crate) sample_rate: u32,
    pub(crate) positions: Vec<AudioChannelPosition>,
}

/// Первая аудиодорожка demuxer-а.
pub(crate) fn first_audio_track(demuxer: &dyn Demuxer) -> TrackInfo {
    demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .unwrap_or_else(|| panic!("audio track expected: {:?}", demuxer.tracks()))
}

/// Пакеты дорожки до EOS; временная неготовность фонового demuxer-а переживается.
pub(crate) fn collect_track_packets(demuxer: &mut dyn Demuxer, track: &TrackInfo) -> Vec<Packet> {
    let deadline = Instant::now() + DEMUX_DEADLINE;
    let mut packets = Vec::new();
    while Instant::now() < deadline {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => packets.push(packet),
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => thread::sleep(READINESS_POLL_INTERVAL),
            DemuxReadEvent::EndOfStream => return packets,
        }
    }
    panic!("demux did not reach EOS before deadline");
}

/// Декодирует пакеты production-декодером, созданным так же, как это делает player-core.
pub(crate) fn decode_track_packets(track: &TrackInfo, packets: &[Packet]) -> DecodedPcm {
    let config = AudioDecoderConfig::from_track_metadata(
        track.id.get(),
        track.codec_id.clone(),
        track.sample_rate,
        track.channels,
    )
    .with_codec_private(track.codec_private.as_ref().map(|bytes| bytes.to_vec()));
    let mut decoder = create_audio_decoder(config).expect("production AC-3 family decoder");
    let mut interleaved = Vec::new();
    for packet in packets {
        let samples = decoder
            .decode(&EncodedAudioPacket::without_timing(
                packet.track_id.get(),
                &packet.data,
            ))
            .expect("decode packet");
        interleaved.extend_from_slice(&samples);
    }
    let spec = decoder.decoded_output_spec().expect("decoded spec");
    let layout = spec.channel_layout;
    let positions = (0..layout.channel_count() as usize)
        .map(|lane| layout.position_at(lane).expect("positioned layout"))
        .collect();
    DecodedPcm {
        interleaved,
        sample_rate: spec.sample_rate,
        positions,
    }
}

/// Энергия тона в одной lane (Гёрцель, нормированный на квадрат длины).
pub(crate) fn tone_energy(decoded: &DecodedPcm, lane: usize, frequency: f32) -> f32 {
    let channels = decoded.positions.len();
    let coefficient = 2.0 * (2.0 * PI * frequency / decoded.sample_rate as f32).cos();
    let (mut previous, mut before_previous, mut count) = (0.0_f32, 0.0_f32, 0_usize);
    for sample in decoded.interleaved.iter().skip(lane).step_by(channels) {
        let current = sample + coefficient * previous - before_previous;
        before_previous = previous;
        previous = current;
        count += 1;
    }
    let power = previous * previous + before_previous * before_previous
        - coefficient * previous * before_previous;
    power / (count as f32 * count as f32)
}

/// Раскладка совпадает с `expected`, и в lane каждой позиции звучит её тон.
pub(crate) fn assert_channel_tones(decoded: &DecodedPcm, expected: &[(AudioChannelPosition, f32)]) {
    assert_eq!(
        decoded.positions.len(),
        expected.len(),
        "{:?}",
        decoded.positions
    );
    assert_tones_in_lanes(decoded, expected);
}

/// В lane каждой перечисленной позиции звучит её тон, а чужие тоны из списка
/// слабее минимум на 20 дБ. Позиции вне списка не проверяются.
///
/// Одинаковые частоты в разных lanes допустимы (TS-фикстуры: 440 Гц везде) — тогда
/// проверяется только присутствие тона.
pub(crate) fn assert_tones_in_lanes(
    decoded: &DecodedPcm,
    expected: &[(AudioChannelPosition, f32)],
) {
    for (position, tone) in expected {
        let lane = decoded
            .positions
            .iter()
            .position(|candidate| candidate == position)
            .unwrap_or_else(|| {
                panic!("decoded layout lacks {position:?}: {:?}", decoded.positions)
            });
        let own = tone_energy(decoded, lane, *tone);
        assert!(
            own > MINIMUM_OWN_TONE_ENERGY,
            "{position:?}: tone {tone} Hz is missing (energy {own})"
        );
        for (_, other_tone) in expected.iter().filter(|(_, other)| other != tone) {
            let foreign = tone_energy(decoded, lane, *other_tone);
            assert!(
                own > foreign * FOREIGN_TONE_REJECTION,
                "{position:?}: {other_tone} Hz leaks into the {tone} Hz lane"
            );
        }
    }
}
