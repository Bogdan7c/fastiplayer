// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Seek в audio-only Opus MKV (ffmpeg: кластеры по ~5 с, Cues с `CueRelativePosition`).
//!
//! Дефект живой приёмки: seek садился на начало СЛЕДУЮЩЕГО кластера (в 2–5 с от цели), у конца
//! сразу давал EOF, а для дорожки, начинающейся позже нуля, seek назад отказывал. Корень был в
//! MKV-патче: `CueTime` трактовался как наносекунды и повторно делился на `TimestampScale`,
//! поэтому кластер после cue-перехода получал timestamp ≈ 0 и чтение вперёд пропускало его.
//!
//! Тест проверяет результат целиком: демуксер честно сообщает точку приземления, первый packet
//! покрывает цель, а настоящий Opus-декодер слышит писк именно той медиа-секунды, которую
//! обещают pts (частота писка кодирует номер секунды, см. `fixtures/matroska/README.md`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use audio::decoder::{
    AudioDecoder, AudioDecoderConfig, AudioPacketTimeBase, AudioPacketTiming, EncodedAudioPacket,
    create_audio_decoder,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackInfo, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Частота дискретизации фикстур.
const SAMPLE_RATE_HZ: f64 = 48_000.0;
/// Окно поиска начала писка: 5 мс.
const ONSET_WINDOW_SAMPLES: usize = 240;
/// RMS тона 440 Гц ≈ 0.085, RMS тона с писком ≈ 0.36: порог между ними.
const BEEP_RMS_THRESHOLD: f32 = 0.2;
/// Отступ от начала писка до окна измерения частоты (уходим от фронта).
const FREQUENCY_WINDOW_OFFSET_SAMPLES: usize = 240;
/// Окно измерения частоты: 30 мс → разрешение ~33 Гц при шаге писков 250 Гц.
const FREQUENCY_WINDOW_SAMPLES: usize = 1_440;
/// Сколько звука декодировать в поиске писка: больше секунды гарантирует попадание на писк.
const BEEP_SEARCH_LIMIT: Duration = Duration::from_millis(1_300);
/// Допуск времени начала писка: окно поиска 5 мс + округление CodecDelay до тика 1 мс +
/// прогрев декодера. Меньше 13 мс, поэтому ловит и прежнее прибавление CodecDelay к pts
/// (звук на 2·6.5 мс позже своего pts), не только промах seek-а на секунды.
const BEEP_ONSET_TOLERANCE: Duration = Duration::from_millis(8);
/// Защитный предел событий демуксера в одном поиске.
const MAX_EVENTS_PER_SEARCH: usize = 1_000;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/matroska")
        .join(name)
}

/// Открытая фикстура: демуксер и её единственная аудиодорожка.
struct AudioFixture {
    demuxer: SymphoniaDemuxer,
    audio_track: TrackInfo,
}

impl AudioFixture {
    fn open(name: &str) -> Self {
        let demuxer = SymphoniaDemuxer::from_file(&fixture(name)).expect("fixture must open");
        let audio_track = demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .cloned()
            .expect("fixture has an audio track");
        assert_eq!(audio_track.channels, Some(1), "beep fixtures are mono");
        Self {
            demuxer,
            audio_track,
        }
    }

    /// Accurate seek, как его делает player-core для audio-only медиа; возвращает приземление.
    fn seek(&mut self, target: Duration) -> SeekLanding {
        let seek_result = self
            .demuxer
            .seek_with_request(DemuxSeekRequest::accurate(target))
            .unwrap_or_else(|error| panic!("seek to {target:?} failed: {error:#}"));
        assert_eq!(
            seek_result.requested_position.as_duration(),
            target,
            "seek result must keep the user target"
        );
        let first_packet = self
            .next_audio_packet()
            .unwrap_or_else(|| panic!("seek to {target:?} immediately hit end of stream"));
        SeekLanding {
            target,
            reported: seek_result.actual_position.as_duration(),
            first_packet,
        }
    }

    /// Следующий packet аудиодорожки или `None` на конце потока.
    fn next_audio_packet(&mut self) -> Option<Packet> {
        for _ in 0..MAX_EVENTS_PER_SEARCH {
            match self.demuxer.next_event().expect("demux read") {
                DemuxReadEvent::Packet(packet) if packet.track_id == self.audio_track.id => {
                    return Some(packet);
                }
                DemuxReadEvent::Packet(_)
                | DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_) => {}
                DemuxReadEvent::TemporarilyUnavailable(hint) => {
                    panic!("local file cannot be temporarily unavailable: {hint:?}")
                }
                DemuxReadEvent::EndOfStream => return None,
            }
        }
        panic!("no audio packet within {MAX_EVENTS_PER_SEARCH} demux events")
    }

    /// Свежий декодер, как после seek-а в плеере (сброс состояния кодека).
    fn fresh_decoder(&self) -> Box<dyn AudioDecoder> {
        let config = AudioDecoderConfig::from_track_metadata(
            self.audio_track.id.get(),
            self.audio_track.codec_id.clone(),
            self.audio_track.sample_rate,
            self.audio_track.channels,
        )
        .with_codec_private(
            self.audio_track
                .codec_private
                .as_ref()
                .map(|bytes| bytes.to_vec()),
        );
        create_audio_decoder(config).expect("opus decoder")
    }

    /// Декодирует звук от точки приземления и находит первый писк после неё.
    fn first_beep_after(&mut self, landing: &SeekLanding) -> HeardBeep {
        let mut decoder = self.fresh_decoder();
        let mut pcm = decoder
            .decode(&encoded_packet(&landing.first_packet))
            .expect("decode first packet");
        let search_limit_samples = (BEEP_SEARCH_LIMIT.as_secs_f64() * SAMPLE_RATE_HZ) as usize;
        while pcm.len() < search_limit_samples {
            let Some(packet) = self.next_audio_packet() else {
                break;
            };
            pcm.extend(
                decoder
                    .decode(&encoded_packet(&packet))
                    .expect("decode packet"),
            );
        }
        let onset_sample = find_beep_onset(&pcm).unwrap_or_else(|| {
            panic!(
                "no beep heard within {BEEP_SEARCH_LIMIT:?} after seek to {:?}",
                landing.target
            )
        });
        HeardBeep {
            onset: landing.first_packet.pts
                + Duration::from_secs_f64(onset_sample as f64 / SAMPLE_RATE_HZ),
            media_second_mod_8: beep_second_mod_8(&pcm, onset_sample),
        }
    }
}

/// Что демуксер сообщил о seek-е и что реально отдал первым.
struct SeekLanding {
    target: Duration,
    reported: Duration,
    first_packet: Packet,
}

impl SeekLanding {
    /// Приземление честное: отчёт совпадает с первым packet-ом, а тот покрывает цель.
    fn assert_first_packet_covers_target(&self) {
        let packet_start = self.first_packet.pts;
        let packet_duration = self.first_packet.duration.expect("opus packet duration");
        assert_eq!(
            self.reported, packet_start,
            "seek to {:?}: reported landing differs from the first delivered packet",
            self.target
        );
        assert!(
            packet_start <= self.target && self.target < packet_start + packet_duration,
            "seek to {:?} landed on packet {:?}+{:?} that does not cover the target",
            self.target,
            packet_start,
            packet_duration
        );
    }
}

/// Писк, услышанный декодером: время начала по pts и номер секунды по частоте.
struct HeardBeep {
    onset: Duration,
    media_second_mod_8: u64,
}

impl HeardBeep {
    fn assert_is_beep_of_second(&self, media_second: u64) {
        let expected_onset = Duration::from_secs(media_second);
        assert_eq!(
            self.media_second_mod_8,
            media_second % 8,
            "decoder heard the beep of another media second (expected second {media_second})"
        );
        assert!(
            self.onset.abs_diff(expected_onset) <= BEEP_ONSET_TOLERANCE,
            "beep of second {media_second} starts at {:?} by pts: pts do not match the content",
            self.onset
        );
    }
}

fn encoded_packet(packet: &Packet) -> EncodedAudioPacket<'_> {
    let timing = packet
        .track_pts
        .and_then(|track_pts| {
            let time_base =
                AudioPacketTimeBase::new(track_pts.time_base.numer, track_pts.time_base.denom)?;
            Some(AudioPacketTiming::from_track_units(
                time_base,
                track_pts.units.get(),
                packet.track_dts.map(|track_dts| track_dts.units.get()),
                packet.track_duration.map(|duration| duration.units.get()),
            ))
        })
        .unwrap_or_else(AudioPacketTiming::unknown);
    EncodedAudioPacket::new(packet.track_id.get(), timing, &packet.data)
}

/// Первое 5-мс окно, где громкость превышает уровень одного тона, — начало писка.
fn find_beep_onset(pcm: &[f32]) -> Option<usize> {
    pcm.as_chunks::<ONSET_WINDOW_SAMPLES>()
        .0
        .iter()
        .position(|window| rms(window) > BEEP_RMS_THRESHOLD)
        .map(|window_index| window_index * ONSET_WINDOW_SAMPLES)
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Номер секунды (mod 8) по частоте писка 1000 + 250·k Гц: самая сильная из восьми частот.
fn beep_second_mod_8(pcm: &[f32], onset_sample: usize) -> u64 {
    let window_start = onset_sample + FREQUENCY_WINDOW_OFFSET_SAMPLES;
    let window = pcm
        .get(window_start..window_start + FREQUENCY_WINDOW_SAMPLES)
        .expect("decoded audio covers the beep measurement window");
    (0..8_u64)
        .max_by(|left, right| {
            goertzel_power(window, beep_frequency_hz(*left))
                .total_cmp(&goertzel_power(window, beep_frequency_hz(*right)))
        })
        .expect("eight candidate frequencies")
}

fn beep_frequency_hz(media_second_mod_8: u64) -> f64 {
    1_000.0 + 250.0 * media_second_mod_8 as f64
}

/// Мощность одной частоты (алгоритм Гёрцеля).
fn goertzel_power(window: &[f32], frequency_hz: f64) -> f64 {
    let coefficient = 2.0 * (2.0 * std::f64::consts::PI * frequency_hz / SAMPLE_RATE_HZ).cos();
    let (mut previous, mut before_previous) = (0.0_f64, 0.0_f64);
    for sample in window {
        let current = f64::from(*sample) + coefficient * previous - before_previous;
        before_previous = previous;
        previous = current;
    }
    previous * previous + before_previous * before_previous
        - coefficient * previous * before_previous
}

fn secs(seconds: f64) -> Duration {
    Duration::from_secs_f64(seconds)
}

/// Вперёд внутрь кластера 4.981–9.981 с, назад в первый кластер и в последний кластер:
/// каждый раз приземление на кадр цели и писк ровно следующей секунды.
#[test]
fn audio_only_opus_mkv_seek_lands_on_target_and_plays_matching_audio() {
    let mut fixture = AudioFixture::open("opus-audio-only-beeps.mka");

    // (цель, медиа-секунда следующего писка).
    for (target_seconds, next_beep_second) in [(7.3, 8), (3.62, 4), (10.5, 11)] {
        let landing = fixture.seek(secs(target_seconds));
        landing.assert_first_packet_covers_target();
        fixture
            .first_beep_after(&landing)
            .assert_is_beep_of_second(next_beep_second);
    }
}

/// Seek у самого конца (97 %) не превращается в EOF: до конца файла остаются все кадры.
#[test]
fn audio_only_opus_mkv_seek_near_end_keeps_remaining_audio() {
    let mut fixture = AudioFixture::open("opus-audio-only-beeps.mka");
    let media_end = fixture.demuxer.duration().expect("fixture duration");
    let target = secs(11.7);

    let landing = fixture.seek(target);
    landing.assert_first_packet_covers_target();

    // Пакеты идут подряд от цели до конца медиа, без скачка к последнему кадру.
    let mut expected_next_pts = landing.first_packet.pts + Duration::from_millis(20);
    let mut packets_after_landing = 0_usize;
    while let Some(packet) = fixture.next_audio_packet() {
        assert!(
            packet.pts.abs_diff(expected_next_pts) <= Duration::from_millis(1),
            "packet {:?} breaks the 20 ms cadence after near-end seek",
            packet.pts
        );
        expected_next_pts = packet.pts + Duration::from_millis(20);
        packets_after_landing += 1;
    }
    let expected_packets = ((media_end - target).as_secs_f64() / 0.02) as usize;
    assert!(
        packets_after_landing + 1 >= expected_packets,
        "only {packets_after_landing} packets after near-end seek, expected about {expected_packets}"
    );
}

/// Дорожка начинается на 2-й секунде: seek назад раньше её начала приземляется на первый
/// кадр (честно позже цели), а seek внутрь второго кластера и назад работают как обычно.
#[test]
fn late_start_opus_mkv_seek_before_track_start_and_inside_clusters() {
    let mut fixture = AudioFixture::open("opus-audio-only-beeps-late2.mka");
    // Первый кадр дорожки, как его отдаёт чтение с начала файла: блок 2.001 с минус CodecDelay
    // 6.5 мс (ffprobe: 1.994 с). Точку приземления сравниваем с ним, а не с «круглыми» 2 с.
    let track_first_frame_pts = fixture
        .next_audio_packet()
        .expect("late track has audio")
        .pts;

    // Уходим вперёд, затем seek раньше начала дорожки.
    let forward = fixture.seek(secs(7.3));
    forward.assert_first_packet_covers_target();
    fixture
        .first_beep_after(&forward)
        .assert_is_beep_of_second(8);

    let before_start = fixture.seek(secs(1.2));
    assert_eq!(
        before_start.reported, before_start.first_packet.pts,
        "landing before track start must report the first delivered packet"
    );
    assert_eq!(
        before_start.reported, track_first_frame_pts,
        "seek before track start must land on its first frame"
    );
    fixture
        .first_beep_after(&before_start)
        .assert_is_beep_of_second(2);

    let backward = fixture.seek(secs(3.4));
    backward.assert_first_packet_covers_target();
    fixture
        .first_beep_after(&backward)
        .assert_is_beep_of_second(4);
}
