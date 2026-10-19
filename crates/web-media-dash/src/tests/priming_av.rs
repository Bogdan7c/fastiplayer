//! Функциональный тест: static ffmpeg MPD, где audio timeline начинается с
//! priming-offset и выступает за конец Period, открывается и реально играет.
//!
//! Сегменты настоящие и не редактировались (ffmpeg `-f dash`, 16x16 H.264 + AAC,
//! `-itsoffset 0.2287` для audio), поэтому тест проходит весь путь: план -> HTTP ->
//! demux -> пакеты. Семантика ffmpeg: сдвиг audio записан пустой правкой `elst`
//! init-сегмента (`segment_duration = 9954`, `media_time = -1`), а `tfdt` сегментов
//! начинается с 0; `S@t` в MPD равен этому сдвигу. Присутствие audio в нужный момент
//! обеспечивает isomp4 demux, который применяет ведущую пустую правку; без неё
//! audio стартовало бы на 0.2 с раньше видео.
//!
//! Регенерация фикстур (ffmpeg 8.x), затем каждый файл кодируется base64 -w0:
//! `ffmpeg -f lavfi -i testsrc=d=2:s=64x64:r=24 -itsoffset 0.2287 -f lavfi
//!  -i sine=frequency=440:sample_rate=48000:d=2.02 -map 0:v -map 1:a
//!  -vf scale=16:16,format=yuv420p -c:v libx264 -profile:v baseline -level 1.0
//!  -g 24 -keyint_min 24 -sc_threshold 0 -c:a aac -b:a 24k -f dash -seg_duration 1
//!  -use_template 1 -use_timeline 1 -init_seg_name 'init-$RepresentationID$.m4s'
//!  -media_seg_name 'chunk-$RepresentationID$-$Number%05d$.m4s' manifest.mpd`

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dash_mpd_core::{DashContainer, DashMediaKind};
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use source_core::{CancellationToken, HttpRequestTarget};
use web_media_transport_api::SourceGeneration;

use super::{
    DashPresentationSelection, DashVodHttpContext, DashVodInput, DashVodOpenRequest,
    adaptive_context, decode_base64, demux_registry, evidence, manifest_input, open_policy,
    prepare_dash_vod,
};

/// Period = 2 с. Video ровно 2 с. Audio идёт с 0.2074 с (сдвиг 9954 тика) и
/// кончается на 2.25 с: последний сегмент (`t=96994`) лежит целиком за Period.
const PRIMING_MPD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
	profiles="urn:mpeg:dash:profile:isoff-live:2011" type="static"
	mediaPresentationDuration="PT2.0S" maxSegmentDuration="PT1.0S" minBufferTime="PT2.0S">
	<Period id="0" start="PT0.0S">
		<AdaptationSet id="0" contentType="video" startWithSAP="1" segmentAlignment="true" frameRate="24/1" maxWidth="16" maxHeight="16">
			<Representation id="0" mimeType="video/mp4" codecs="avc1.42c00a" bandwidth="12772" width="16" height="16">
				<SegmentTemplate timescale="12288" initialization="init-$RepresentationID$.m4s" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1">
					<SegmentTimeline><S t="0" d="12288" r="1" /></SegmentTimeline>
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
		<AdaptationSet id="1" contentType="audio" startWithSAP="1" segmentAlignment="true">
			<Representation id="1" mimeType="audio/mp4" codecs="mp4a.40.2" bandwidth="24000" audioSamplingRate="48000">
				<SegmentTemplate timescale="48000" initialization="init-$RepresentationID$.m4s" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1">
					<SegmentTimeline>
						<S t="9954" d="38912" />
						<S d="48128" />
						<S d="10944" />
					</SegmentTimeline>
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
	</Period>
</MPD>"#;

/// HTTP-сервер из фиксированной карты `путь -> тело`; помнит запрошенные пути.
struct RoutedServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    requested_paths: Arc<Mutex<Vec<String>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RoutedServer {
    fn start(routes: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind routed fixture");
        listener.set_nonblocking(true).expect("nonblocking fixture");
        let address = listener.local_addr().expect("fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let requested_paths = Arc::new(Mutex::new(Vec::new()));
        let (worker_stop, worker_paths) = (Arc::clone(&stop), Arc::clone(&requested_paths));
        let routes = Arc::new(routes);
        let thread = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request_bytes = [0_u8; 4 * 1_024];
                        let read = stream.read(&mut request_bytes).expect("read request");
                        let request = String::from_utf8_lossy(&request_bytes[..read]);
                        let path = request
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or_default()
                            .to_owned();
                        worker_paths.lock().expect("paths lock").push(path.clone());
                        let response = match routes.get(&path) {
                            Some(body) => {
                                let mut response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    body.len()
                                )
                                .into_bytes();
                                response.extend_from_slice(body);
                                response
                            }
                            None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                        };
                        stream.write_all(&response).expect("write response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("routed fixture accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            requested_paths,
            thread: Some(thread),
        }
    }

    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("routed fixture target")
    }

    fn was_requested(&self, path: &str) -> bool {
        self.requested_paths
            .lock()
            .expect("paths lock")
            .iter()
            .any(|requested| requested == path)
    }
}

impl Drop for RoutedServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("routed fixture stops");
        }
    }
}

fn priming_routes() -> HashMap<String, Vec<u8>> {
    let mut routes = HashMap::new();
    routes.insert("/manifest.mpd".to_owned(), PRIMING_MPD.as_bytes().to_vec());
    macro_rules! route {
        ($path:literal, $fixture:literal) => {
            routes.insert(
                $path.to_owned(),
                decode_base64(include_str!(concat!("../../tests/fixtures/", $fixture))),
            );
        };
    }
    route!("/init-0.m4s", "dash-priming-video-init.base64");
    route!("/chunk-0-00001.m4s", "dash-priming-video-one.base64");
    route!("/chunk-0-00002.m4s", "dash-priming-video-two.base64");
    route!("/init-1.m4s", "dash-priming-audio-init.base64");
    route!("/chunk-1-00001.m4s", "dash-priming-audio-one.base64");
    route!("/chunk-1-00002.m4s", "dash-priming-audio-two.base64");
    route!("/chunk-1-00003.m4s", "dash-priming-audio-three.base64");
    routes
}

/// Результат полного чтения demuxer-а до EndOfStream.
struct DrainedPackets {
    video_pts: Vec<Duration>,
    audio_pts: Vec<Duration>,
    reached_end_of_stream: bool,
}

fn drain_until_end(demuxer: &mut dyn Demuxer) -> DrainedPackets {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut drained = DrainedPackets {
        video_pts: Vec::new(),
        audio_pts: Vec::new(),
        reached_end_of_stream: false,
    };
    while Instant::now() < deadline {
        match demuxer.next_event().expect("DASH runtime event") {
            DemuxReadEvent::Packet(packet) => match packet.kind {
                TrackKind::Video => drained.video_pts.push(packet.pts),
                TrackKind::Audio => drained.audio_pts.push(packet.pts),
                _ => {}
            },
            DemuxReadEvent::EndOfStream => {
                drained.reached_end_of_stream = true;
                break;
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => thread::sleep(Duration::from_millis(1)),
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
        }
    }
    drained
}

#[test]
fn ffmpeg_audio_priming_and_overshoot_mpd_opens_and_plays_both_components_in_sync() {
    let server = RoutedServer::start(priming_routes());
    let manifest_target = server.target("/manifest.mpd");
    let generation = SourceGeneration::new(1);
    let result = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::Manifest(Box::new(adaptive_context(
            &manifest_target,
            CancellationToken::new(),
            generation,
        ))),
        generation,
        input: DashVodInput::Manifest(manifest_input(manifest_target)),
        selection: DashPresentationSelection::Separate {
            video: evidence(DashMediaKind::Video, DashContainer::IsoBmff, Some("0")),
            audio: evidence(DashMediaKind::Audio, DashContainer::IsoBmff, Some("1")),
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    })
    .expect("MPD with audio priming offset and Period overshoot must open");

    // Duration = Period, а не конец audio timeline (2.25 с).
    assert_eq!(result.duration(), Duration::from_secs(2));
    let mut demuxer = result.into_demuxer();
    let kinds = demuxer
        .tracks()
        .iter()
        .map(|track| track.kind)
        .collect::<Vec<_>>();
    assert!(kinds.contains(&TrackKind::Video) && kinds.contains(&TrackKind::Audio));

    let drained = drain_until_end(&mut demuxer);
    assert!(
        drained.reached_end_of_stream,
        "playback must reach EndOfStream"
    );
    assert!(
        !drained.video_pts.is_empty(),
        "video packets must be delivered"
    );
    assert!(
        !drained.audio_pts.is_empty(),
        "audio packets must be delivered"
    );

    // Sync: video стартует в 0, audio — в позиции пустой правки `elst` (9954 тика при
    // 48 кГц), которая совпадает с `S@t` первого audio сегмента в MPD.
    let first_video = *drained.video_pts.iter().min().expect("video packet");
    let first_audio = *drained.audio_pts.iter().min().expect("audio packet");
    let expected_audio_start = Duration::from_secs_f64(9954.0 / 48_000.0);
    assert!(
        first_video < Duration::from_millis(1),
        "video starts at 0: {first_video:?}"
    );
    let audio_error = first_audio.abs_diff(expected_audio_start);
    assert!(
        audio_error < Duration::from_millis(1),
        "audio must start at the elst dwell (== MPD S@t, 0.207375 s): {first_audio:?}"
    );
    // Сегмент, лежащий целиком за Period, не планируется и не скачивается.
    assert!(!server.was_requested("/chunk-1-00003.m4s"));
}
