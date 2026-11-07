//! Поддельная страница экстеншена протокола 1.1 для сквозных тестов HLS-поколения
//! (10a1): раздаёт ресурсы по номеркам, как это будет делать фон экстеншена (10a2).
//!
//! Страница отвечает на каждый `fetch_resource` рамками вида 2 (не больше 1 МиБ, с
//! нуля подряд) и `resource_fetch_finished`. Ресурсы — тела из фикстур репозитория,
//! последовательные снимки (live-плейлист) или отказ (`refused`, HTTP-ошибка). Каждый
//! полученный запрос записывается: тест проверяет, ЧТО плеер просил (только номерки).

use std::collections::{HashMap, HashSet};

use base64::Engine;
use browser_bridge_protocol::{
    ByteOffset, HttpStatusCode, PageSessionMessage, RangeBytesHeader, RangeRequestId,
    ResourceBodySize, ResourceByteRange, ResourceFetchFailure, ResourceFetchOutcome,
    encode_message,
};

use super::*;

/// Начало адреса-заглушки номерка (как его переписывает экстеншен).
pub(super) const BRIDGE_ADDRESS_PREFIX: &str = "https://resource.fastiplayer-bridge.invalid/";

/// Самая большая рамка байтов (протокол: ≤ 1 МиБ).
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// `hello` экстеншена протокола 1.1.
pub(super) const HELLO_1_1: &str = r#"{"type":"hello","protocol":{"major":1,"minor":1},"software_version":"0.1.0","capabilities":[]}"#;

/// Каталог HLS-фикстур приложения (base64, сегменты по 1 с, 16×16, 10 к/с).
const HLS_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/media_open/web/tests/fixtures"
);

/// Фрагментированный MP4 из `pushed-media-source`: 4 с, 25 к/с, 100 кадров, H.264 +
/// AAC, десять фрагментов по 0,4 с (каждый с ключевого кадра).
const FRAGMENTED_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../pushed-media-source/fixtures/file-h264-aac-fragmented.mp4"
);

/// Адрес-заглушка номерка.
pub(super) fn bridge_address(handle: u32) -> String {
    format!("{BRIDGE_ADDRESS_PREFIX}{handle}")
}

/// Сырые байты base64-фикстуры HLS.
pub(super) fn hls_fixture(name: &str) -> Vec<u8> {
    let encoded = std::fs::read_to_string(format!("{HLS_FIXTURES}/{name}.base64"))
        .expect("HLS fixture is readable");
    let compact: String = encoded.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact)
        .expect("HLS fixture is valid base64")
}

/// Фрагментированный MP4, разрезанный на init (`ftyp`+`moov`) и фрагменты (`moof`+`mdat`).
pub(super) struct FragmentedMovie {
    /// `ftyp` + `moov`.
    pub(super) init: Vec<u8>,
    /// Фрагменты по 0,4 с, по порядку.
    pub(super) fragments: Vec<Vec<u8>>,
}

/// Длительность одного фрагмента `FragmentedMovie`, с.
pub(super) const FRAGMENT_SECONDS: f64 = 0.4;
/// Кадров в одном фрагменте (25 к/с × 0,4 с).
pub(super) const FRAGMENT_FRAMES: u64 = 10;

/// Режет фикстуру по верхним боксам.
pub(super) fn fragmented_movie() -> FragmentedMovie {
    let bytes = std::fs::read(FRAGMENTED_FIXTURE).expect("fragmented fixture");
    let mut boxes = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        let size = u32::from_be_bytes(bytes[position..position + 4].try_into().expect("size"));
        let kind = bytes[position + 4..position + 8].to_vec();
        let end = position + size as usize;
        boxes.push((kind, bytes[position..end].to_vec()));
        position = end;
    }
    let mut init = Vec::new();
    let mut fragments = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    for (kind, body) in boxes {
        match kind.as_slice() {
            b"ftyp" | b"moov" => init.extend_from_slice(&body),
            b"moof" => {
                if !current.is_empty() {
                    fragments.push(std::mem::take(&mut current));
                }
                current.extend_from_slice(&body);
            }
            _ => current.extend_from_slice(&body),
        }
    }
    if !current.is_empty() {
        fragments.push(current);
    }
    assert_eq!(fragments.len(), 10, "the fixture has ten fragments");
    FragmentedMovie { init, fragments }
}

/// Плейлист VOD fMP4: init — номерок `init`, сегменты — номерки `first_segment..`.
pub(super) fn fmp4_vod_playlist(init: u32, first_segment: u32, segments: usize) -> Vec<u8> {
    let mut text = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"{}\"\n",
        bridge_address(init)
    );
    for index in 0..segments {
        text.push_str(&format!(
            "#EXTINF:{FRAGMENT_SECONDS:.3},\n{}\n",
            bridge_address(first_segment + index as u32)
        ));
    }
    text.push_str("#EXT-X-ENDLIST\n");
    text.into_bytes()
}

/// Что страница отдаёт по номерку.
#[derive(Clone)]
pub(super) enum PageResource {
    /// Неизменное тело.
    Body(Vec<u8>),
    /// N-й запрос получает N-й снимок, после последнего — последний (live-корень).
    Snapshots(Vec<Vec<u8>>),
    /// Фон отказался качать.
    Refused,
    /// Сервер ответил HTTP-ошибкой.
    HttpError(u16),
    /// У фона оборвалась сеть.
    NetworkError,
}

/// Один `fetch_resource`, который получила страница.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ServedRequest {
    /// Номер запроса.
    pub(super) request_id: u32,
    /// Номерок ресурса.
    pub(super) resource: u32,
    /// Кусок `(offset, length)` или весь ресурс (`None`).
    pub(super) slice: Option<(u64, u64)>,
}

/// Поддельная страница: ресурсы по номеркам и журнал запросов.
pub(super) struct HlsPage {
    resources: HashMap<u32, PageResource>,
    /// Сколько раз отдавали каждый снимок-ресурс.
    snapshot_turns: HashMap<u32, usize>,
    /// Все полученные запросы по порядку.
    pub(super) requests: Vec<ServedRequest>,
    /// Номерки, на которые страница пока молчит (медленный сервер): запросы ждут.
    held_handles: HashSet<u32>,
    /// Запросы удержанных номерков, ещё без ответа.
    held_requests: Vec<PlayerSessionMessage>,
}

impl HlsPage {
    /// Страница с этими ресурсами.
    pub(super) fn new(resources: impl IntoIterator<Item = (u32, PageResource)>) -> Self {
        Self {
            resources: resources.into_iter().collect(),
            snapshot_turns: HashMap::new(),
            requests: Vec::new(),
            held_handles: HashSet::new(),
            held_requests: Vec::new(),
        }
    }

    /// Страница молчит на эти номерки, пока тест не отпустит их ([`Self::release_held`]).
    pub(super) fn hold(&mut self, handles: impl IntoIterator<Item = u32>) {
        self.held_handles.extend(handles);
    }

    /// Номерки больше не удерживаются; ждавшие запросы получают ответ сейчас (у брошенных
    /// плеером это поздние рамки — плеер их молча выбросит).
    pub(super) fn release_held(&mut self, socket: &mut WebSocket<MaybeTlsStream<TcpStream>>) {
        self.held_handles.clear();
        for message in std::mem::take(&mut self.held_requests) {
            self.respond(socket, &message);
        }
    }

    /// Номерки запросов, пришедших после `since` запросов (порядок прихода).
    pub(super) fn handles_requested_after(&self, since: usize) -> Vec<u32> {
        self.requests[since..]
            .iter()
            .map(|request| request.resource)
            .collect()
    }

    /// Меняет (или добавляет) ресурс посреди теста (403 на сегменте посреди просмотра).
    pub(super) fn replace(&mut self, handle: u32, resource: PageResource) {
        self.resources.insert(handle, resource);
    }

    /// Сколько раз плеер просил номерок.
    pub(super) fn requests_for(&self, handle: u32) -> usize {
        self.requests
            .iter()
            .filter(|request| request.resource == handle)
            .count()
    }

    /// Отвечает на `fetch_resource`; прочие сообщения пропускает.
    pub(super) fn answer(
        &mut self,
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        message: &PlayerSessionMessage,
    ) {
        let PlayerSessionMessage::FetchResource {
            request_id,
            resource,
            byte_range,
            ..
        } = message
        else {
            return;
        };
        let slice = match byte_range {
            ResourceByteRange::Whole {} => None,
            ResourceByteRange::Slice { offset, length } => {
                Some((offset.as_bytes(), length.as_bytes()))
            }
        };
        self.requests.push(ServedRequest {
            request_id: request_id.0,
            resource: resource.0,
            slice,
        });
        if self.held_handles.contains(&resource.0) {
            self.held_requests.push(message.clone());
            return;
        }
        self.respond(socket, message);
    }

    /// Ответ на один `fetch_resource` (уже записанный в журнал запросов).
    fn respond(
        &mut self,
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        message: &PlayerSessionMessage,
    ) {
        let PlayerSessionMessage::FetchResource {
            request_id,
            resource,
            byte_range,
            max_bytes,
        } = message
        else {
            return;
        };
        let slice = match byte_range {
            ResourceByteRange::Whole {} => None,
            ResourceByteRange::Slice { offset, length } => {
                Some((offset.as_bytes(), length.as_bytes()))
            }
        };
        let body = match self.resources.get(&resource.0).cloned() {
            Some(PageResource::Body(body)) => body,
            Some(PageResource::Snapshots(snapshots)) => {
                let turn = self.snapshot_turns.entry(resource.0).or_insert(0);
                let snapshot = snapshots
                    .get(*turn)
                    .or_else(|| snapshots.last())
                    .cloned()
                    .unwrap_or_default();
                *turn += 1;
                snapshot
            }
            Some(PageResource::Refused) => {
                return finish(
                    socket,
                    *request_id,
                    failed(ResourceFetchFailure::Refused {}),
                );
            }
            Some(PageResource::NetworkError) => {
                return finish(
                    socket,
                    *request_id,
                    failed(ResourceFetchFailure::NetworkError {}),
                );
            }
            Some(PageResource::HttpError(status)) => {
                let status = HttpStatusCode::new(status).expect("HTTP status");
                return finish(
                    socket,
                    *request_id,
                    failed(ResourceFetchFailure::HttpError { status }),
                );
            }
            None => {
                let status = HttpStatusCode::new(404).expect("HTTP status");
                return finish(
                    socket,
                    *request_id,
                    failed(ResourceFetchFailure::HttpError { status }),
                );
            }
        };
        let body = match slice {
            None => body,
            Some((offset, length)) => {
                let start = (offset as usize).min(body.len());
                let end = (start + length as usize).min(body.len());
                body[start..end].to_vec()
            }
        };
        if body.len() as u64 > max_bytes.as_bytes() {
            return finish(
                socket,
                *request_id,
                failed(ResourceFetchFailure::TooLarge {}),
            );
        }
        let mut position = 0;
        while position < body.len() {
            let frame_end = (position + MAX_FRAME_BYTES).min(body.len());
            let header = DataFrameHeader::RangeBytes(RangeBytesHeader {
                request_id: *request_id,
                offset: ByteOffset::new(position as u64).expect("offset"),
            });
            let frame =
                encode_data_frame(&header, &body[position..frame_end]).expect("resource frame");
            socket.send(Message::binary(frame)).expect("resource bytes");
            position = frame_end;
        }
        let completed = ResourceFetchOutcome::Completed {
            size_bytes: ResourceBodySize::new(body.len() as u64).expect("size"),
        };
        finish(socket, *request_id, completed);
    }

    /// Отвечает на запросы, пока `stop` не скажет «хватит»; возвращает всё прочитанное.
    pub(super) fn serve_until(
        &mut self,
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        what: &str,
        mut stop: impl FnMut(&Self, &PlayerSessionMessage) -> bool,
    ) -> Vec<PlayerSessionMessage> {
        let deadline = Instant::now() + WAIT;
        let mut messages = Vec::new();
        loop {
            assert!(
                Instant::now() < deadline,
                "the page never saw {what}; requests so far: {:?}",
                self.requests
            );
            let batch = read_player_messages_until(socket, what, |_| true);
            for message in batch {
                self.answer(socket, &message);
                let done = stop(self, &message);
                messages.push(message);
                if done {
                    return messages;
                }
            }
        }
    }

    /// Отвечает, пока плеер не доиграет (часы `ended`).
    pub(super) fn serve_until_ended(
        &mut self,
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    ) -> Vec<PlayerSessionMessage> {
        self.serve_until(socket, "the end of the HLS stream", |_, message| {
            matches!(
                message,
                PlayerSessionMessage::Clock {
                    state: ClockState::Ended,
                    ..
                }
            )
        })
    }
}

/// Итог «не удалось».
const fn failed(failure: ResourceFetchFailure) -> ResourceFetchOutcome {
    ResourceFetchOutcome::Failed { failure }
}

/// `resource_fetch_finished` странице → плееру.
fn finish(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    request_id: RangeRequestId,
    outcome: ResourceFetchOutcome,
) {
    let finished = PageSessionMessage::ResourceFetchFinished {
        request_id,
        outcome,
    };
    let text = encode_message(&finished).expect("resource_fetch_finished");
    socket.send(Message::text(text)).expect("resource result");
}

/// Будит процесс, открывает сеанс с `hello` этой версии и подключается, как экстеншен.
pub(super) fn open_attached_session_speaking(
    process: &SessionProcessUnderTest,
    hello: &str,
) -> WebSocket<MaybeTlsStream<TcpStream>> {
    let bus = &process.bus;
    let client = BrowserEntryClient::connect(
        BrowserEntryBus::Address(bus.address.clone()),
        BrowserEntryClientConfig::default(),
    )
    .expect("D-Bus activation starts fastiplayer --browser-session");
    assert!(matches!(
        entry_call(&client, hello),
        PlayerControlMessage::HelloAccepted(_)
    ));
    let PlayerControlMessage::SessionGranted(grant) = entry_call(&client, OPEN_SESSION) else {
        panic!("open_session must be granted");
    };
    bus.wait_for_log_line("Browser session opened");
    drop(client);
    let socket = attach(&grant);
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(WAIT))
            .expect("read timeout is set");
    }
    if process.mode == VideoMode::Window {
        bus.wait_for_log_line("Browser window ready");
    }
    socket
}

/// Объявляет поколение 1 как HLS с корнем `root`.
pub(super) fn announce_hls_generation(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    root: u32,
) {
    let hls_source = format!(r#"{{"type":"hls_source_added","generation":1,"root":{root}}}"#);
    for text in [
        r#"{"type":"generation_started","generation":1}"#,
        hls_source.as_str(),
    ] {
        socket.send(Message::text(text)).expect("message is sent");
    }
}

/// Итог плеера: показанные кадры, откаты позиции, сыгранный звук.
pub(super) struct PlaybackSummary {
    pub(super) presented_frames: u64,
    pub(super) position_regressions: u64,
    pub(super) audio_played_ms: u64,
    pub(super) line: String,
}

/// Уходит вкладка (если ещё не ушла) и читается итог плеера.
pub(super) fn final_summary(bus: &PrivateBusWithBrowserSession) -> PlaybackSummary {
    let line = bus.wait_for_log_line("Browser playback summary kind=final");
    PlaybackSummary {
        presented_frames: log_number(&line, "presented_frames"),
        position_regressions: log_number(&line, "position_regressions"),
        audio_played_ms: log_number(&line, "audio_played_ms"),
        line,
    }
}
