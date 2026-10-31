//! Общая обвязка loopback-тестов: окружение, клиент, ожидания.

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use browser_bridge_protocol::{
    ExtensionControlMessage, OpenSessionRequest, PageSessionMessage, SessionKey, SessionToken,
    decode_message,
};
use browser_bridge_session::{
    BrowserSessionPolicy, BrowserSessionServer, ChunkSource, EventSinkFull, OpenedSession,
    SessionByteSink, SessionChunk, SessionControl, SessionEventSink, SessionLifecycleEvent,
    SessionRef, SessionTokenSource, TokenSourceError,
};
use tungstenite::{Message, WebSocket};

/// Потолок ожидания: щедрый, чтобы медленная машина не давала ложных падений.
pub const WAIT: Duration = Duration::from_secs(20);

/// Значение, за изменениями которого можно ждать (условная переменная).
pub struct Watched<T> {
    value: Mutex<T>,
    changed: Condvar,
}

impl<T: Clone + Default> Watched<T> {
    pub fn new() -> Self {
        Self {
            value: Mutex::new(T::default()),
            changed: Condvar::new(),
        }
    }

    pub fn update(&self, change: impl FnOnce(&mut T)) {
        change(&mut self.value.lock().unwrap());
        self.changed.notify_all();
    }

    pub fn get(&self) -> T {
        self.value.lock().unwrap().clone()
    }

    /// Ждёт, пока условие выполнится; по потолку падает с `what`.
    pub fn wait_for(&self, what: &str, condition: impl Fn(&T) -> bool) -> T {
        let deadline = Instant::now() + WAIT;
        let mut guard = self.value.lock().unwrap();
        while !condition(&guard) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "не дождались: {what}");
            guard = self.changed.wait_timeout(guard, left).unwrap().0;
        }
        guard.clone()
    }
}

/// Принятые байты по (поколение, SourceBuffer) и по запросам диапазонов.
#[derive(Clone, Default)]
pub struct ReceivedBytes {
    pub source_buffers: BTreeMap<(u32, u16), Vec<u8>>,
    pub ranges: BTreeMap<u32, Vec<u8>>,
    pub frames: usize,
    pub ended: Vec<SessionRef>,
    /// Кусков, пришедших уже ПОСЛЕ `session_ended` своего сеанса (должно быть 0).
    pub late_chunks: usize,
}

#[derive(Default)]
pub struct RecordingByteSink {
    pub received: Watched<ReceivedBytes>,
}

impl SessionByteSink for RecordingByteSink {
    fn accept(&self, chunk: SessionChunk<'_>) {
        self.received.update(|received| {
            received.frames += 1;
            if received.ended.contains(chunk.session) {
                received.late_chunks += 1;
            }
            match chunk.source {
                ChunkSource::SourceBuffer(buffer) => received
                    .source_buffers
                    .entry((buffer.generation.get(), buffer.source_buffer.get()))
                    .or_default()
                    .extend_from_slice(buffer.payload),
                ChunkSource::Range(range) => received
                    .ranges
                    .entry(range.request_id.0)
                    .or_default()
                    .extend_from_slice(range.payload),
            }
        });
    }

    fn session_ended(&self, session: &SessionRef) {
        self.received
            .update(|received| received.ended.push(session.clone()));
    }
}

impl Default for Watched<ReceivedBytes> {
    fn default() -> Self {
        Self::new()
    }
}

/// События, собранные приёмником.
#[derive(Clone, Default)]
pub struct SeenEvents {
    pub lifecycle: Vec<SessionLifecycleEvent>,
    pub page_messages: Vec<PageSessionMessage>,
}

pub struct RecordingEvents {
    pub seen: Watched<SeenEvents>,
    /// `true` — приёмник всегда «переполнен» (проверка счётчика потерь).
    pub always_full: bool,
}

impl SessionEventSink for RecordingEvents {
    fn publish_page_message(
        &self,
        _session: &SessionRef,
        message: PageSessionMessage,
    ) -> Result<(), EventSinkFull> {
        if self.always_full {
            return Err(EventSinkFull);
        }
        self.seen.update(|seen| seen.page_messages.push(message));
        Ok(())
    }

    fn publish_lifecycle(&self, event: SessionLifecycleEvent) {
        self.seen.update(|seen| seen.lifecycle.push(event));
    }
}

/// Пропуска «000…01», «000…02» …
pub struct SequenceTokens(pub u32);

impl SessionTokenSource for SequenceTokens {
    fn next_token(&mut self) -> Result<SessionToken, TokenSourceError> {
        self.0 += 1;
        Ok(SessionToken::new(format!("{:032}", self.0))?)
    }
}

/// Политика тестов: миллисекундный опрос, остальные сроки задаёт тест.
pub fn test_policy() -> BrowserSessionPolicy {
    BrowserSessionPolicy {
        connection_poll_interval: Duration::from_millis(5),
        attach_timeout: Duration::from_secs(10),
        first_attach_deadline: Duration::from_secs(30),
        reconnect_grace: Duration::from_secs(30),
        close_linger: Duration::from_secs(1),
        ..BrowserSessionPolicy::default()
    }
}

/// Сервер с записывающими приёмниками.
pub struct Env {
    pub server: BrowserSessionServer,
    pub control: SessionControl,
    pub bytes: Arc<RecordingByteSink>,
    pub events: Arc<RecordingEvents>,
}

impl Env {
    pub fn start() -> Self {
        Self::start_with(test_policy())
    }

    pub fn start_with(policy: BrowserSessionPolicy) -> Self {
        Self::start_full(policy, false)
    }

    pub fn start_full(policy: BrowserSessionPolicy, events_always_full: bool) -> Self {
        let bytes = Arc::new(RecordingByteSink::default());
        let events = Arc::new(RecordingEvents {
            seen: Watched::new(),
            always_full: events_always_full,
        });
        let server = BrowserSessionServer::start(
            policy,
            Box::new(SequenceTokens(0)),
            Arc::clone(&bytes) as Arc<dyn SessionByteSink>,
            Arc::clone(&events) as Arc<dyn SessionEventSink>,
        )
        .unwrap();
        let control = server.control();
        Self {
            server,
            control,
            bytes,
            events,
        }
    }

    /// Открывает сеанс как «стойка входа».
    pub fn open(&self, key: &str) -> OpenedSession {
        self.control
            .open_session(&open_request(key), Instant::now())
            .unwrap()
    }

    pub fn port(&self) -> u16 {
        self.server.port().get()
    }

    /// Ждёт событие жизни по условию.
    pub fn wait_lifecycle(&self, what: &str, find: impl Fn(&SessionLifecycleEvent) -> bool) {
        self.events
            .seen
            .wait_for(what, |seen| seen.lifecycle.iter().any(&find));
    }
}

/// Запрос `open_session` на основе фикстуры протокола с подменой ключа.
pub fn open_request(key: &str) -> OpenSessionRequest {
    let json = include_str!(
        "../../../browser-bridge-protocol/fixtures/control/valid/open-session-window.json"
    )
    .replace("tab-12-video-3", key);
    match decode_message::<ExtensionControlMessage>(json.as_bytes()).unwrap() {
        ExtensionControlMessage::OpenSession(request) => request,
        other => panic!("ожидали open_session: {other:?}"),
    }
}

pub fn key(text: &str) -> SessionKey {
    SessionKey::new(text).unwrap()
}

/// Сеанс по ключу и номеру экземпляра. Экземпляры в тестах считаются с 1.
pub fn session(text: &str, instance: u64) -> SessionRef {
    SessionRef {
        key: key(text),
        instance: browser_bridge_session::SessionInstance(instance),
    }
}

/// Клиент WebSocket: подключается к loopback и проходит рукопожатие.
pub fn connect(port: u16) -> WebSocket<TcpStream> {
    try_connect(port).expect("рукопожатие WebSocket не удалось")
}

pub fn try_connect(port: u16) -> Result<WebSocket<TcpStream>, String> {
    let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|error| error.to_string())?;
    stream.set_read_timeout(Some(WAIT)).unwrap();
    tungstenite::client(format!("ws://127.0.0.1:{port}/"), stream)
        .map(|(socket, _response)| socket)
        .map_err(|error| error.to_string())
}

pub fn send_text(socket: &mut WebSocket<TcpStream>, json: &str) {
    socket.send(Message::text(json)).unwrap();
}

pub fn send_binary(socket: &mut WebSocket<TcpStream>, frame: Vec<u8>) {
    socket.send(Message::binary(frame)).unwrap();
}

pub fn attach_json(session: &str, token: &SessionToken) -> String {
    format!(
        r#"{{"type":"attach","session_key":"{session}","token":"{}"}}"#,
        token.as_str()
    )
}

/// Подключается и проходит `attach`; возвращает клиент после `attached`.
pub fn attach(port: u16, session: &str, token: &SessionToken) -> WebSocket<TcpStream> {
    let mut socket = connect(port);
    send_text(&mut socket, &attach_json(session, token));
    assert_eq!(read_text(&mut socket), r#"{"type":"attached"}"#);
    socket
}

/// Читает следующее текстовое сообщение (пинги и пр. пропускает).
pub fn read_text(socket: &mut WebSocket<TcpStream>) -> String {
    loop {
        match socket.read().expect("соединение оборвалось, ожидали текст")
        {
            Message::Text(text) => return text.as_str().to_owned(),
            Message::Close(frame) => panic!("ожидали текст, пришло закрытие {frame:?}"),
            _ => {}
        }
    }
}

/// Ждёт кадр закрытия и возвращает его код.
pub fn read_close_code(socket: &mut WebSocket<TcpStream>) -> u16 {
    loop {
        match socket.read() {
            Ok(Message::Close(Some(frame))) => {
                // Как браузер: сразу отвечаем кадром закрытия, чтобы сервер не ждал.
                let _ = socket.flush();
                return u16::from(frame.code);
            }
            Ok(Message::Close(None)) => panic!("закрытие без кода"),
            Ok(_) => {}
            Err(error) => panic!("ожидали кадр закрытия, получили ошибку: {error}"),
        }
    }
}

/// Двоичная рамка протокола (вид 1) с грузом в 1 КиБ; порядковый номер подставляет вызывающий.
pub fn data_frame_for(generation: u32, source_buffer: u16) -> Vec<u8> {
    let mut frame = vec![0_u8; 24 + 1024];
    frame[0] = 1;
    frame[4..8].copy_from_slice(&generation.to_le_bytes());
    frame[8..10].copy_from_slice(&source_buffer.to_le_bytes());
    frame
}

/// Читает всё текстовое до кадра закрытия: (тексты по порядку, код закрытия).
/// Так тест видит ПОРЯДОК «сообщения → закрытие» целиком.
pub fn read_texts_until_close(socket: &mut WebSocket<TcpStream>) -> (Vec<String>, u16) {
    let mut texts = Vec::new();
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => texts.push(text.as_str().to_owned()),
            Ok(Message::Close(Some(frame))) => {
                // Как браузер: сразу отвечаем кадром закрытия.
                let _ = socket.flush();
                return (texts, u16::from(frame.code));
            }
            Ok(Message::Close(None)) => panic!("закрытие без кода"),
            Ok(_) => {}
            Err(error) => panic!("ожидали тексты и кадр закрытия, получили ошибку: {error}"),
        }
    }
}

/// Шлёт ping и ждёт pong с тем же грузом. Сервер отвечает на ping, только когда
/// прочитал его, а читает он по порядку: после pong всё, что клиент послал до
/// ping, сервером уже прочитано и обработано.
pub fn round_trip_ping(socket: &mut WebSocket<TcpStream>, marker: &[u8]) {
    socket.send(Message::Ping(marker.to_vec().into())).unwrap();
    loop {
        match socket.read().expect("соединение оборвалось, ожидали pong")
        {
            Message::Pong(payload) if payload.as_ref() == marker => return,
            Message::Close(frame) => panic!("ожидали pong, пришло закрытие {frame:?}"),
            _ => {}
        }
    }
}

/// Политика, при которой поток соединения подолгу «стоит» в чтении сокета.
/// Нужна, чтобы тест надёжно ставил сообщения в очередь и просил закрыть, пока
/// поток спит: старый код (закрытие раньше сброса очереди) при этом стабильно
/// терял очередь. Новому коду пауза безразлична.
pub fn parked_reader_policy() -> BrowserSessionPolicy {
    BrowserSessionPolicy {
        connection_poll_interval: Duration::from_millis(200),
        ..test_policy()
    }
}

/// Даёт потоку соединения уснуть в чтении (см. `parked_reader_policy`). Это не
/// условие правильности, а способ воспроизвести старую гонку детерминированно.
pub fn let_the_connection_thread_park() {
    std::thread::sleep(Duration::from_millis(40));
}

/// Ожидаемый JSON прощания — фикстура протокола.
pub const RETURN_TO_PAGE_JSON: &str = include_str!(
    "../../../browser-bridge-protocol/fixtures/session/valid/player-return-to-page.json"
);

/// Прощание «вернись на 1029 с, пауза» — ровно фикстура `player-return-to-page`.
pub fn return_to_page() -> browser_bridge_protocol::PlayerSessionMessage {
    browser_bridge_protocol::PlayerSessionMessage::ReturnToPage {
        position_us: browser_bridge_protocol::MediaTime::from_micros(1_029_000_000).unwrap(),
        resume: browser_bridge_protocol::session::ResumeState::Paused,
    }
}
