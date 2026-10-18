//! Общие помощники тестов встроенных дорожек: реальные пакеты из маленьких файлов-фикстур
//! `symphonia-demux` превращаются в сообщения трубы и подаются владельцу.

use std::path::{Path, PathBuf};

use media_core::{DemuxReadEvent, Demuxer, MediaAttachments, Packet, TrackInfo, TrackKind};
use player_core::{
    SubtitleFeedMediaStart, SubtitleFeedMessage, SubtitleFeedPacket, SubtitleReadRun,
    SubtitleTrackHeader, TrackSummarySnapshot,
};
use symphonia_demux::SymphoniaDemuxer;

use super::*;

// Типы владельца, которые нужны всем тестам встроенных дорожек.
pub(super) use super::super::EmbeddedUpdate;
pub(super) use super::super::intents::{CycleOutcome, SelectTrackOutcome};
pub(super) use super::super::menu_view::{SubtitleTrackAvailability, SubtitleUnavailableReason};
pub(super) use super::super::track_list::TrackLoadState;

pub(super) const GENERATION: u64 = 7;

/// Всё, что плеер узнаёт из файла: дорожки, пакеты субтитров и вложения.
pub(super) struct DemuxedFixture {
    /// Строки снимка плеера (видео, звук и субтитры, как в настоящем снимке).
    pub(super) tracks: Vec<TrackSummarySnapshot>,
    /// Пакеты субтитровых дорожек в порядке файла.
    pub(super) subtitle_packets: Vec<Packet>,
    /// Вложения контейнера.
    pub(super) attachments: MediaAttachments,
    /// Заголовки субтитровых дорожек (`codec_private`), как их собирает `player-core`.
    pub(super) track_headers: Vec<SubtitleTrackHeader>,
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../symphonia-demux/tests/fixtures/subtitles")
        .join(name)
}

/// Строка снимка плеера по описанию дорожки демуксера.
fn snapshot_row(track: &TrackInfo) -> TrackSummarySnapshot {
    TrackSummarySnapshot {
        id: track.id,
        kind: track.kind,
        codec_id: track.codec_id.clone(),
        sample_rate: track.sample_rate,
        channels: track.channels,
        duration: track.duration,
        video: track.video.clone(),
        video_color_summary: None,
        labels: track.labels.clone(),
    }
}

/// Читает фикстуру настоящим демуксером.
pub(super) fn demux_fixture(name: &str) -> DemuxedFixture {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture_path(name)).expect("fixture opens");
    let tracks = demuxer.tracks().iter().map(snapshot_row).collect();
    let attachments = demuxer.take_attachments();
    let track_headers = demuxer
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Subtitle)
        .filter_map(|track| {
            track
                .codec_private
                .clone()
                .filter(|header| !header.is_empty())
                .map(|codec_private| SubtitleTrackHeader {
                    track_id: track.id,
                    codec_private,
                })
        })
        .collect();
    let mut subtitle_packets = Vec::new();
    loop {
        match demuxer.next_event().expect("read demux event") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Subtitle => {
                subtitle_packets.push(packet);
            }
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    DemuxedFixture {
        tracks,
        subtitle_packets,
        attachments,
        track_headers,
    }
}

impl DemuxedFixture {
    /// Строка снимка n-й субтитровой дорожки.
    pub(super) fn subtitle_row(&self, nth: usize) -> &TrackSummarySnapshot {
        self.tracks
            .iter()
            .filter(|row| row.kind == TrackKind::Subtitle)
            .nth(nth)
            .expect("subtitle track exists")
    }

    /// Сообщения трубы для всех пакетов фикстуры (как их отправляет session).
    pub(super) fn feed_messages(&self, media: MediaInstanceId) -> Vec<SubtitleFeedMessage> {
        self.subtitle_packets
            .iter()
            .map(|packet| packet_message(media, packet))
            .collect()
    }

    /// Сообщение «началось media» со вложениями фикстуры.
    pub(super) fn media_started(&self, media: MediaInstanceId) -> SubtitleFeedMessage {
        SubtitleFeedMessage::MediaStarted(SubtitleFeedMediaStart {
            media_instance_id: media,
            attachments: self.attachments.clone(),
            track_headers: self.track_headers.clone(),
        })
    }
}

/// Пакет демуксера -> сообщение трубы (те же поля, что копирует `player-core`).
pub(super) fn packet_message(media: MediaInstanceId, packet: &Packet) -> SubtitleFeedMessage {
    SubtitleFeedMessage::Packet(SubtitleFeedPacket {
        media_instance_id: media,
        track_id: packet.track_id,
        presentation_time: packet.pts,
        duration: packet.duration,
        read_run: SubtitleReadRun::INITIAL,
        payload: packet.data.clone(),
    })
}

/// Сообщение с произвольной нагрузкой для дорожки (тесты ошибок и пределов).
pub(super) fn raw_packet(
    media: MediaInstanceId,
    track_id: media_core::TrackId,
    at_ms: u64,
    duration_ms: Option<u64>,
    payload: &[u8],
) -> SubtitleFeedMessage {
    SubtitleFeedMessage::Packet(SubtitleFeedPacket {
        media_instance_id: media,
        track_id,
        presentation_time: Duration::from_millis(at_ms),
        duration: duration_ms.map(Duration::from_millis),
        read_run: SubtitleReadRun::INITIAL,
        payload: bytes::Bytes::copy_from_slice(payload),
    })
}

/// Ждёт, пока выполнится условие, опрашивая фоновые задачи владельца.
pub(super) fn wait_until(
    owner: &mut SubtitleOwner,
    what: &str,
    done: impl Fn(&SubtitleOwner) -> bool,
) {
    let deadline = Instant::now() + LOAD_TIMEOUT;
    while !done(owner) {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Владелец с media без файла рядом и со списком дорожек из `fixture`; растеризатор готов.
pub(super) fn embedded_owner(
    config: &SubtitlesConfig,
    media: MediaInstanceId,
    fixture: &DemuxedFixture,
) -> SubtitleOwner {
    let mut owner = owner_with_config(config);
    owner.begin_media(media, SubtitleMediaSource::NotLocal);
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);
    wait_until(&mut owner, "rasterizer", |owner| {
        owner.presenter.has_rasterizer()
    });
    owner
}

/// Число реплик в кэше встроенной дорожки плеера `player_track`.
pub(super) fn cached_cues(owner: &SubtitleOwner, player_track: media_core::TrackId) -> usize {
    let media = owner.media.as_ref().expect("media");
    let id = media
        .tracks
        .find_embedded(player_track)
        .expect("embedded track in the list");
    match &media.tracks.get(id).expect("entry").load {
        TrackLoadState::Ready(track) => track.timeline().len(),
        _ => panic!("embedded track must be Ready"),
    }
}

/// Идентификатор дорожки владельца по идентификатору плеера.
pub(super) fn owner_track_id(
    owner: &SubtitleOwner,
    player_track: media_core::TrackId,
) -> subtitle_core::SubtitleTrackId {
    owner
        .media
        .as_ref()
        .and_then(|media| media.tracks.find_embedded(player_track))
        .expect("embedded track in the list")
}

/// Тексты активных реплик показываемой дорожки на кадре `pts_ms`.
pub(super) fn visible_texts(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    pts_ms: u64,
) -> usize {
    let rect = full_rect(1280, 720);
    owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .map_or(0, |overlay| overlay.bitmaps().len())
}
