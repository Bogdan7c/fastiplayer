//! Прямая HTTP-ссылка на сырой `.eac3` / `.ac3`: production `classify_direct_media_url`
//! → `open_direct_media` (HTTP Range transport → prefetch → web demux composition →
//! `ac3-elementary-demux` → фоновый `ProgressiveDemuxer`) → пакеты → production
//! `audio::create_audio_decoder` → тон в каждой lane. Плюс seek через seek port
//! (как player) по Range и честный forward-only путь для сервера без Range.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fastiplayer_config::{NetworkConfig, PlayerDemuxConfig};
use media_core::{DemuxSeekRequest, DemuxSeekability, MediaTime};
use player_core::{PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, PreparedDemuxSeekRequestId};
use source_core::CancellationToken;

use super::{DirectProgressiveRuntimeParts, classify_direct_media_url, open_direct_media};
use crate::ac3_test_support::{
    SAMPLES_PER_ACCESS_UNIT, STEREO_AC3, STEREO_TONES, SURROUND_EAC3, SURROUND_TONES,
    assert_channel_tones, collect_track_packets, decode_track_packets, first_audio_track,
};

/// Пакетов в каждой сырой фикстуре (0.5 с по 32 мс).
const FIXTURE_FRAMES: usize = 16;
/// Цель seek-а: середина E-AC-3 фикстуры; ближайший кадр не позже цели — №7 (224 мс).
const MIDDLE_SEEK_TARGET: Duration = Duration::from_millis(250);
/// Индекс кадра, на который обязан сесть seek к `MIDDLE_SEEK_TARGET`.
const MIDDLE_SEEK_FRAME: usize = 7;
/// Длительность одного access unit при 48 кГц.
const ACCESS_UNIT_DURATION: Duration = Duration::from_millis(32);
/// Срок ожидания квитанции seek-а от фонового demux-потока.
const SEEK_RECEIPT_DEADLINE: Duration = Duration::from_secs(10);

/// Как origin отвечает на GET.
#[derive(Clone, Copy)]
enum OriginMode {
    /// `206 Partial Content` по заголовку `Range` (seekable ресурс).
    RangeRequests,
    /// Всегда `200` с полным телом и без `Accept-Ranges` (forward-only ресурс).
    FullBodyOnly,
}

/// Loopback origin одного файла с подсчётом запросов.
struct DolbyOrigin {
    address: SocketAddr,
    file_name: &'static str,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    worker: Option<JoinHandle<()>>,
}

impl DolbyOrigin {
    fn spawn(file_name: &'static str, body: &'static [u8], mode: OriginMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind Dolby origin");
        let address = listener.local_addr().expect("Dolby origin address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_requests = Arc::clone(&requests);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let request = read_request_head(&mut stream);
                if request.is_empty() {
                    continue;
                }
                worker_requests.fetch_add(1, Ordering::SeqCst);
                respond(&mut stream, &request, body, mode);
            }
        });
        Self {
            address,
            file_name,
            stop,
            requests,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/{}", self.address, self.file_name)
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for DolbyOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Будим blocking accept, чтобы worker увидел stop.
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Читает request line и headers до пустой строки.
fn read_request_head(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

/// `Range: bytes=<start>-[<end>]` → включительный интервал внутри тела.
fn requested_range(request: &str, body_length: usize) -> Option<(usize, usize)> {
    let value = request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("range").then(|| value.trim())
    })?;
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let end = if end.is_empty() {
        body_length - 1
    } else {
        end.parse::<usize>().ok()?.min(body_length - 1)
    };
    Some((start, end))
}

fn respond(stream: &mut TcpStream, request: &str, body: &[u8], mode: OriginMode) {
    let range = match mode {
        OriginMode::RangeRequests => requested_range(request, body.len()),
        OriginMode::FullBodyOnly => None,
    };
    let (head, payload) = match range {
        Some((start, end)) => (
            format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/eac3\r\n\
                 Content-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\n\
                 Accept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                end + 1 - start,
                body.len()
            ),
            &body[start..=end],
        ),
        None => (
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/eac3\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            ),
            body,
        ),
    };
    // Клиент может закрыть соединение раньше (prefetch/cancel) — это не ошибка теста.
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload);
}

/// Production classification + open, как это делает приложение для прямой ссылки.
fn open_direct(url: &str) -> DirectProgressiveRuntimeParts {
    let locator = classify_direct_media_url(url).expect("raw Dolby URL is direct media");
    open_direct_media(
        &locator,
        &NetworkConfig::default(),
        &PlayerDemuxConfig::default(),
        CancellationToken::new(),
    )
    .expect("direct raw Dolby open")
    .into_runtime_parts()
}

/// Seek через seek port фонового demux-потока (как player) и ожидание квитанции.
fn seek_through_port(seek_port: &dyn PreparedDemuxSeekPort, position: Duration) -> MediaTime {
    let request_id = PreparedDemuxSeekRequestId::new(1);
    seek_port
        .enqueue_seek(request_id, DemuxSeekRequest::accurate(position))
        .expect("seekable direct runtime accepts seek");
    let deadline = Instant::now() + SEEK_RECEIPT_DEADLINE;
    loop {
        if let Some(receipt) = seek_port.poll_seek_receipt() {
            assert_eq!(receipt.request_id, request_id);
            let PreparedDemuxSeekOutcome::Succeeded(result) = receipt.outcome else {
                panic!("seek must succeed: {:?}", receipt.outcome);
            };
            return result.actual_position;
        }
        assert!(Instant::now() < deadline, "direct seek receipt timeout");
        thread::sleep(Duration::from_millis(1));
    }
}

/// HTTP Range `.eac3` 5.1 → `A_EAC3` → PCM с тоном в каждой из шести lanes.
#[test]
fn http_range_eac3_reaches_production_decoder_in_every_channel() {
    let origin = DolbyOrigin::spawn("surround.eac3", SURROUND_EAC3, OriginMode::RangeRequests);
    let mut parts = open_direct(&origin.url());
    assert_eq!(parts.demuxer.seekability(), DemuxSeekability::Seekable);
    assert!(
        parts.demux_seek_port.is_some(),
        "Range resource gets a seek port"
    );
    let track = first_audio_track(parts.demuxer.as_ref());
    assert_eq!(track.codec_id, "A_EAC3");
    assert_eq!((track.sample_rate, track.channels), (Some(48_000), Some(6)));
    assert_eq!(
        parts.demuxer.duration(),
        Some(Duration::from_millis(512)),
        "Content-Length gives the CBR duration"
    );

    let packets = collect_track_packets(parts.demuxer.as_mut(), &track);
    assert_eq!(packets.len(), FIXTURE_FRAMES);
    let decoded = decode_track_packets(&track, &packets);
    assert_eq!(
        decoded.interleaved.len(),
        FIXTURE_FRAMES * SAMPLES_PER_ACCESS_UNIT * 6
    );
    assert_channel_tones(&decoded, &SURROUND_TONES);
}

/// Seek по HTTP Range в середину: посадка на кадр 7, хвост декодируется целиком.
#[test]
fn http_range_eac3_middle_seek_lands_on_frame_and_decodes_tail() {
    let origin = DolbyOrigin::spawn("seek.eac3", SURROUND_EAC3, OriginMode::RangeRequests);
    let mut parts = open_direct(&origin.url());
    let track = first_audio_track(parts.demuxer.as_ref());
    let seek_port = parts.demux_seek_port.clone().expect("seek port");

    let landed = seek_through_port(seek_port.as_ref(), MIDDLE_SEEK_TARGET);
    let expected_landing = ACCESS_UNIT_DURATION * MIDDLE_SEEK_FRAME as u32;
    assert_eq!(landed, MediaTime::from_duration(expected_landing));

    let packets = collect_track_packets(parts.demuxer.as_mut(), &track);
    assert_eq!(packets.len(), FIXTURE_FRAMES - MIDDLE_SEEK_FRAME);
    assert_eq!(packets[0].pts, expected_landing);
    let decoded = decode_track_packets(&track, &packets);
    assert_channel_tones(&decoded, &SURROUND_TONES);
}

/// Сервер без Range: тот же `.ac3` играет forward-only одним GET, seek честно отклонён.
#[test]
fn http_full_body_ac3_is_forward_only_and_still_reaches_pcm() {
    let origin = DolbyOrigin::spawn("stereo.ac3", STEREO_AC3, OriginMode::FullBodyOnly);
    let mut parts = open_direct(&origin.url());
    assert!(matches!(
        parts.demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    assert!(
        parts.demux_seek_port.is_none(),
        "forward-only: no seek port"
    );
    let track = first_audio_track(parts.demuxer.as_ref());
    assert_eq!(track.codec_id, "A_AC3");

    let packets = collect_track_packets(parts.demuxer.as_mut(), &track);
    assert_eq!(packets.len(), FIXTURE_FRAMES);
    assert_channel_tones(&decode_track_packets(&track, &packets), &STEREO_TONES);
    assert_eq!(
        origin.request_count(),
        1,
        "the initial 200 body is demuxed without a second GET"
    );
}

/// Classification опирается на настоящие registrations: Dolby-расширения — direct,
/// похожее, но не зарегистрированное (`.ac4`), — typed отказ.
#[test]
fn raw_dolby_extensions_are_classified_as_direct_media() {
    for extension in ["ac3", "eac3", "ec3"] {
        let url = format!("https://media.example/stream/track.{extension}?token=secret");
        let locator = classify_direct_media_url(&url)
            .unwrap_or_else(|error| panic!("{extension} must be direct: {error}"));
        assert_eq!(locator.extension().as_extension_hint(), extension);
    }
    assert!(classify_direct_media_url("https://media.example/track.ac4").is_err());
}
