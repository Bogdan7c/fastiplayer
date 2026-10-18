//! Настоящее открытие нативного HLS с WebVTT-рендицией (loopback-сервер, MPEG-TS видео с
//! ненулевым первым PTS): отдаёт тестам владельца субтитров intent, из которого приложение берёт
//! источник субтитров потока, и время первого пакета видео на шкале плеера.
//!
//! Сквозная проверка (`state::subtitles::tests::stream_hls_vertical`) строится поверх него.

use std::any::Any;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, DemuxRetryHint, Demuxer, TrackKind};
use web_media_hls::HlsVodStartIntent;

use super::super::*;
use super::native_hls_vertical::{
    ControlledHlsServer, fixture_routes, native_settings, prepare_native,
};
use crate::media_open::{NativeHlsUrl, SafeMediaLabel};

/// Первый PTS видео в фикстуре `ts-0.ts`, тики 90 кГц (1,4213 с: сырые метки не с нуля).
pub(crate) const FIXTURE_FIRST_VIDEO_PTS_TICKS: u64 = 127_920;

/// Сколько тиков 90 кГц между первым кадром видео и началом сегмента субтитров (0,5 с): карта
/// времени WebVTT намеренно привязана НЕ к первому кадру, чтобы арифметика карты была видна.
pub(crate) const VTT_MAP_OFFSET_TICKS: u64 = 45_000;

/// Master: одна TS-рендиция видео и группа субтитров с одной английской дорожкой.
const MASTER: &str = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"subs.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=16x16,CODECS=\"avc1.42c00a,mp4a.40.2\",SUBTITLES=\"subs\"\n\
ts.m3u8\n";

/// Плейлист субтитров: один сегмент на всё видео (2 с).
const SUBTITLE_PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n\
#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2.000000,\nsubs-0.vtt\n#EXT-X-ENDLIST\n";

/// Открытый HLS-источник с субтитрами.
pub(crate) struct HlsSubtitleFixture {
    /// Intent открытого media: приложение берёт из него источник субтитров потока.
    pub(crate) intent: WebMediaSourceIntent,
    /// Время первого пакета видео на шкале плеера (после пересчёта демуксером).
    pub(crate) first_video_player_time: Duration,
    /// Сервер и демуксер живут, пока жив фикстурный объект.
    _alive: Box<dyn Any>,
}

/// Сегмент WebVTT с картой времени: `LOCAL:0` соответствует тику `MPEGTS`.
fn vtt_segment(mpegts_ticks: u64) -> Vec<u8> {
    format!(
        "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:{mpegts_ticks},LOCAL:00:00:00.000\n\n\
00:00:00.500 --> 00:00:01.500\nMapped cue\n"
    )
    .into_bytes()
}

/// Открывает HLS через настоящий `prepare_native` и читает демуксер до первого пакета видео:
/// к этому моменту точка отсчёта времени эпохи опубликована, и субтитры могут быть посчитаны.
pub(crate) fn open_hls_with_subtitles() -> HlsSubtitleFixture {
    let mut routes: HashMap<String, Vec<Vec<u8>>> = fixture_routes();
    routes.insert("/master.m3u8".to_owned(), vec![MASTER.as_bytes().to_vec()]);
    routes.insert(
        "/subs.m3u8".to_owned(),
        vec![SUBTITLE_PLAYLIST.as_bytes().to_vec()],
    );
    routes.insert(
        "/subs-0.vtt".to_owned(),
        vec![vtt_segment(
            FIXTURE_FIRST_VIDEO_PTS_TICKS + VTT_MAP_OFFSET_TICKS,
        )],
    );
    let server = ControlledHlsServer::start(routes);
    let source = NativeHlsUrl::new(
        server.target("/master.m3u8"),
        SafeMediaLabel::from_service_safe_label("stream subtitles vertical"),
    );
    let settings = native_settings();
    let mut prepared = prepare_native(&source, None, &settings, HlsVodStartIntent::Beginning);
    let first_video_player_time = first_video_packet_time(prepared.demuxer.as_mut());
    let intent = WebMediaSourceIntent::native_hls(
        source,
        web_media_core::WebMediaPresentationKind::Vod,
        prepared.source_state,
    );
    HlsSubtitleFixture {
        intent,
        first_video_player_time,
        _alive: Box::new((server, prepared.demuxer)),
    }
}

/// Время первого пакета видео (его `pts` уже на шкале плеера).
fn first_video_packet_time(demuxer: &mut dyn Demuxer) -> Duration {
    let video_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
        .expect("the fixture publishes a video track");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == video_track => {
                return packet.pts;
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            DemuxReadEvent::EndOfStream => panic!("no video packet before the end of the stream"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "no video packet in time");
    }
}
