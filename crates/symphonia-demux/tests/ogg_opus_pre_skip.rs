// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Ogg Opus `pre_skip` (RFC 7845 §4.2): первые `pre_skip` декодированных отсчётов — прогрев
//! кодера, а медиа-время отсчёта равно `granule − pre_skip`. Upstream Symphonia Ogg переводил
//! гранулу во время без вычитания `pre_skip`, поэтому первый пакет получал pts 0 вместо −6.5 мс
//! (312 отсчётов при 48 кГц), и весь звук Ogg Opus отставал на 6.5 мс от ffmpeg/mpv и от того же
//! звука в Matroska (где `CodecDelay` уже вычитается из pts).
//!
//! Декодер (наш Opus fallback adapter) прогрев НЕ выбрасывает: сдвиг живёт только в
//! timestamp-ах, как и у Matroska. Поэтому одинаковые pts в двух контейнерах означают
//! одинаковый звук — это и проверяется настоящим декодированием, а не только числами.
//!
//! Фикстуры: `ogg/opus-audio-only-beeps.opus` и `matroska/opus-audio-only-beeps.mka` —
//! один и тот же сигнал и настройки libopus (см. `fixtures/ogg/generate.sh`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use audio::decoder::{
    AudioDecoder, AudioDecoderConfig, AudioPacketTimeBase, AudioPacketTiming, EncodedAudioPacket,
    create_audio_decoder,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackInfo, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Частота дискретизации Opus-фикстур.
const SAMPLE_RATE_HZ: f64 = 48_000.0;
/// `pre_skip` из OpusHead, который пишет ffmpeg/libopus: 312 отсчётов = 6.5 мс.
const FFMPEG_OPUS_PRE_SKIP_SAMPLES: i64 = 312;
/// Окно сравнения звука: 60 мс вокруг начала писка 1-й секунды (громкий, легко различимый).
const COMPARISON_WINDOW_START: Duration = Duration::from_millis(980);
/// Длина окна сравнения в отсчётах (60 мс).
const COMPARISON_WINDOW_SAMPLES: usize = 2_880;
/// Перебираемый сдвиг между контейнерами: ±10 мс (больше прежнего дефекта 6.5 мс).
const MAX_SEARCHED_LAG_SAMPLES: i64 = 480;
/// Допуск совпадения: 1 мс. MKV хранит pts в тиках 1 мс (−6 вместо −6.5 мс), это 24 отсчёта.
const MAX_ALLOWED_LAG_SAMPLES: i64 = 48;
/// Сколько звука декодировать с начала: окно сравнения заканчивается на 1.04 с.
const DECODE_LIMIT: Duration = Duration::from_millis(1_200);
/// Защитный предел событий демуксера при поиске одного packet-а.
const MAX_EVENTS_PER_PACKET: usize = 1_000;

fn fixture(relative_path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative_path)
}

/// Открытая фикстура с единственной аудиодорожкой.
struct OpusFixture {
    demuxer: SymphoniaDemuxer,
    audio_track: TrackInfo,
}

impl OpusFixture {
    fn open(relative_path: &str) -> Self {
        let demuxer =
            SymphoniaDemuxer::from_file(&fixture(relative_path)).expect("fixture must open");
        let audio_track = demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .cloned()
            .expect("fixture has an audio track");
        assert_eq!(
            audio_track.codec_id, "A_OPUS",
            "{relative_path}: Opus track"
        );
        Self {
            demuxer,
            audio_track,
        }
    }

    /// Следующий packet аудиодорожки или `None` на конце потока.
    fn next_audio_packet(&mut self) -> Option<Packet> {
        for _ in 0..MAX_EVENTS_PER_PACKET {
            match self.demuxer.next_event().expect("demux read") {
                DemuxReadEvent::Packet(packet) if packet.track_id == self.audio_track.id => {
                    return Some(packet);
                }
                DemuxReadEvent::EndOfStream => return None,
                DemuxReadEvent::TemporarilyUnavailable(hint) => {
                    panic!("local file cannot be temporarily unavailable: {hint:?}")
                }
                _ => {}
            }
        }
        panic!("no audio packet within {MAX_EVENTS_PER_PACKET} demux events")
    }

    /// Свежий декодер, как его создаёт плеер для дорожки (в том числе после seek-а).
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

    /// Декодирует подряд packet-ы, начиная с `first_packet`, и привязывает PCM к медиа-времени
    /// по signed pts первого packet-а (так звук ставит на шкалу плеер).
    fn decode_from(&mut self, first_packet: Packet) -> TimedPcm {
        let mut decoder = self.fresh_decoder();
        let start_seconds = signed_track_pts_seconds(&first_packet);
        let mut samples = decoder
            .decode(&encoded_packet(&first_packet))
            .expect("decode first packet");
        let limit_samples = (DECODE_LIMIT.as_secs_f64() * SAMPLE_RATE_HZ) as usize;
        while samples.len() < limit_samples {
            let Some(packet) = self.next_audio_packet() else {
                break;
            };
            samples.extend(
                decoder
                    .decode(&encoded_packet(&packet))
                    .expect("decode packet"),
            );
        }
        TimedPcm {
            start_seconds,
            samples,
        }
    }
}

/// Моно PCM, первый отсчёт которого стоит на медиа-времени `start_seconds`.
struct TimedPcm {
    start_seconds: f64,
    samples: Vec<f32>,
}

impl TimedPcm {
    /// Отсчёты окна, начинающегося на медиа-времени `start` + `lag_samples`.
    fn window(&self, start: Duration, lag_samples: i64) -> &[f32] {
        let offset_seconds = start.as_secs_f64() - self.start_seconds;
        let first_index = (offset_seconds * SAMPLE_RATE_HZ).round() as i64 + lag_samples;
        let first_index = usize::try_from(first_index).expect("window starts inside decoded PCM");
        self.samples
            .get(first_index..first_index + COMPARISON_WINDOW_SAMPLES)
            .expect("decoded PCM covers the comparison window")
    }

    /// На сколько отсчётов звук `self` сдвинут относительно `reference` на одной медиа-шкале:
    /// сдвиг с минимальной суммой квадратов разности в окне вокруг писка 1-й секунды.
    /// Положительный результат — звук `self` опаздывает.
    fn lag_against(&self, reference: &Self) -> i64 {
        let reference_window = reference.window(COMPARISON_WINDOW_START, 0);
        (-MAX_SEARCHED_LAG_SAMPLES..=MAX_SEARCHED_LAG_SAMPLES)
            .min_by(|left, right| {
                let left_error = squared_error(
                    self.window(COMPARISON_WINDOW_START, *left),
                    reference_window,
                );
                let right_error = squared_error(
                    self.window(COMPARISON_WINDOW_START, *right),
                    reference_window,
                );
                left_error.total_cmp(&right_error)
            })
            .expect("non-empty lag range")
    }
}

fn squared_error(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left_sample, right_sample)| f64::from(left_sample - right_sample).powi(2))
        .sum()
}

/// Signed pts packet-а в секундах (public `Packet::pts` клампится в 0 и прячет приминг).
fn signed_track_pts_seconds(packet: &Packet) -> f64 {
    let track_pts = packet.track_pts.expect("packet keeps signed track pts");
    track_pts.units.get() as f64 * f64::from(track_pts.time_base.numer)
        / f64::from(track_pts.time_base.denom)
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

/// Звук с начала файла: свежий декодер с первого packet-а.
fn decode_from_start(fixture: &mut OpusFixture) -> TimedPcm {
    let first_packet = fixture.next_audio_packet().expect("fixture has audio");
    fixture.decode_from(first_packet)
}

#[test]
fn ogg_opus_first_packet_starts_pre_skip_before_zero() {
    let mut fixture = OpusFixture::open("ogg/opus-audio-only-beeps.opus");
    let first_packet = fixture.next_audio_packet().expect("fixture has audio");
    let track_pts = first_packet.track_pts.expect("ogg packet keeps signed pts");

    // Шкала Ogg Opus — отсчёты 48 кГц, поэтому сравнение точное: −312, как у ffprobe (−0.0065 с).
    assert_eq!(
        (track_pts.time_base.numer, track_pts.time_base.denom),
        (1, 48_000)
    );
    assert_eq!(track_pts.units.get(), -FFMPEG_OPUS_PRE_SKIP_SAMPLES);
    // Следующий packet идёт сразу за первым (20 мс кадр), сдвиг не теряется по дороге.
    let second_packet = fixture.next_audio_packet().expect("second packet");
    assert_eq!(
        second_packet.track_pts.expect("signed pts").units.get(),
        960 - FFMPEG_OPUS_PRE_SKIP_SAMPLES
    );
}

/// Главная наблюдаемая гарантия: один и тот же звук в Ogg и Matroska стоит на одном и том же
/// медиа-времени (±1 мс). До исправления Ogg опаздывал на 312 отсчётов; двойная коррекция
/// Matroska (CodecDelay в pts + ещё и pre_skip) дала бы такой же сдвиг в другую сторону.
#[test]
fn ogg_and_matroska_opus_place_the_same_audio_at_the_same_media_time() {
    let ogg_pcm = decode_from_start(&mut OpusFixture::open("ogg/opus-audio-only-beeps.opus"));
    let matroska_pcm =
        decode_from_start(&mut OpusFixture::open("matroska/opus-audio-only-beeps.mka"));

    let lag_samples = ogg_pcm.lag_against(&matroska_pcm);

    assert!(
        lag_samples.abs() <= MAX_ALLOWED_LAG_SAMPLES,
        "Ogg Opus audio is shifted by {lag_samples} samples ({:.2} ms) against the same audio in Matroska",
        lag_samples as f64 / SAMPLE_RATE_HZ * 1_000.0
    );
}

/// После seek-а pts тоже в медиа-времени: приземление честное (не позже цели, отчёт совпадает
/// с первым packet-ом), а звук после seek-а совпадает со звуком того же времени при чтении
/// с начала. Без согласованного сдвига цели seek/отчёт расходились бы с pts packet-ов.
#[test]
fn ogg_opus_seek_keeps_packets_on_the_media_timeline() {
    let reference_pcm = decode_from_start(&mut OpusFixture::open("ogg/opus-audio-only-beeps.opus"));
    let mut fixture = OpusFixture::open("ogg/opus-audio-only-beeps.opus");
    let target = Duration::from_millis(500);

    let seek_result = fixture
        .demuxer
        .seek_with_request(DemuxSeekRequest::accurate(target))
        .expect("ogg seek");
    let first_packet = fixture.next_audio_packet().expect("audio after seek");

    assert_eq!(
        seek_result.actual_position.as_duration(),
        first_packet.pts,
        "reported landing differs from the first delivered packet"
    );
    assert!(
        first_packet.pts <= target,
        "seek to {target:?} landed later, at {:?}",
        first_packet.pts
    );
    let after_seek_pcm = fixture.decode_from(first_packet);
    let lag_samples = after_seek_pcm.lag_against(&reference_pcm);
    assert!(
        lag_samples.abs() <= MAX_ALLOWED_LAG_SAMPLES,
        "audio after seek is shifted by {lag_samples} samples against reading from the start"
    );
}
