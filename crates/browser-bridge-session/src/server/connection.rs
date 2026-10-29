//! Поток одного WebSocket-соединения: рукопожатие, `attach`, потом приём
//! сообщений и байтов страницы и отправка команд плеера.
//!
//! Поток владеет сокетом целиком; снаружи на него влияют только две вещи
//! (инвариант «модуль владеет своими данными»): [`CloseRequest`] — «закрой
//! этим кодом» и очередь исходящих сообщений. Читает сокет с короткой паузой
//! (`connection_poll_interval`), поэтому просьбу замечает за один опрос.
//!
//! Что соединение делает с входящим:
//! - первое сообщение — текстовый `attach`, иначе 4003 (или 4007 по таймауту);
//! - текст → `PageSessionMessage` → сторож потока → приёмник событий;
//! - двоичная рамка → `decode_data_frame` → сторож потока → приёмник байт;
//!   устаревшее поколение молча выбрасывается и считается;
//! - любое нарушение (битый кадр, вторая `attach`, пропуск номера) — 4003;
//! - `tab_gone` — сообщение уходит приёмнику, соединение закрывается кодом 4002.

use std::io::Read;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Instant;

use browser_bridge_protocol::limits::{DATA_FRAME_HEADER_BYTES, MAX_DATA_FRAME_PAYLOAD_BYTES};
use browser_bridge_protocol::{
    DataFrameHeader, GateDecision, PageSessionMessage, PlayerSessionMessage, SessionCloseReason,
    StreamGate, decode_data_frame, decode_message, encode_message,
};
use tungstenite::error::ProtocolError;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tungstenite::{HandshakeError, Message, WebSocket, accept_with_config};

use super::shared::{CloseRequest, ConnectionHandle, DeliveryGate, Shared};
use crate::policy::BrowserSessionPolicy;
use crate::registry::{ConnectionId, SessionRef};
use crate::sink::{ChunkSource, RangeChunk, SessionChunk, SourceBufferChunk};

/// Наибольшее сообщение WebSocket: двоичная рамка протокола (заголовок + груз).
/// JSON короче (128 КиБ) и дополнительно проверяется при разборе.
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = DATA_FRAME_HEADER_BYTES + MAX_DATA_FRAME_PAYLOAD_BYTES;

/// Наибольшее сообщение до `attach`: сам `attach` короче 1 КиБ (ключ ≤ 64 Б,
/// пропуск ≤ 128 Б), 8 КиБ с запасом. Не даёт безымянному подключению заставить
/// нас читать мегабайты, пока оно не предъявило пропуск.
const MAX_PRE_ATTACH_MESSAGE_BYTES: usize = 8 * 1024;

/// В журнал пишем первую потерю сообщения страницы и каждую N-ю.
const LOG_EVERY_NTH_DROPPED_MESSAGE: u64 = 100;

/// Пора ли писать в журнал о потере сообщения страницы с таким накопленным числом.
fn should_log_dropped_message(dropped_total: u64) -> bool {
    dropped_total == 1 || dropped_total.is_multiple_of(LOG_EVERY_NTH_DROPPED_MESSAGE)
}

/// Тело потока соединения.
pub(super) fn run(shared: &Arc<Shared>, stream: TcpStream, handle: ConnectionHandle) {
    let connection = handle.id;
    serve(shared, stream, handle);
    shared.release_connection(connection);
}

fn serve(shared: &Arc<Shared>, stream: TcpStream, handle: ConnectionHandle) {
    let accepted_at = Instant::now();
    if let Err(error) = configure_stream(&stream, &shared.policy) {
        tracing::warn!(%error, "cannot configure browser session socket");
        return;
    }
    let Some(socket) = handshake(shared, stream, &handle, accepted_at) else {
        return;
    };
    let mut link = Link {
        shared,
        id: handle.id,
        close: handle.close,
        delivery: handle.delivery,
        outgoing: handle.outgoing,
        socket,
    };
    if let Some(session) = link.await_attach(accepted_at) {
        link.serve_attached(&session);
    }
}

/// Тайм-ауты сокета: чтение короткими шагами, запись с потолком.
fn configure_stream(stream: &TcpStream, policy: &BrowserSessionPolicy) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(policy.connection_poll_interval))?;
    stream.set_write_timeout(Some(policy.write_timeout))
}

/// Рукопожатие WebSocket в пределах `handshake_timeout`. `None` — соединение негодно
/// (не WebSocket, оборвано, просрочено, сервер выключается); код закрытия тут
/// невозможен — WebSocket ещё не установлен, поэтому просто бросаем TCP.
fn handshake(
    shared: &Shared,
    stream: TcpStream,
    handle: &ConnectionHandle,
    accepted_at: Instant,
) -> Option<WebSocket<TcpStream>> {
    // Origin НЕ проверяем: у Firefox он — UUID на установку (решение 82), охраняет пропуск.
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_PRE_ATTACH_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_PRE_ATTACH_MESSAGE_BYTES));
    // Рукопожатие HTTP (до WebSocket) короче общего срока attach.
    let deadline = accepted_at
        + shared
            .policy
            .handshake_timeout
            .min(shared.policy.attach_timeout);
    let mut attempt = accept_with_config(stream, Some(config));
    loop {
        match attempt {
            Ok(socket) => return Some(socket),
            Err(HandshakeError::Failure(error)) => {
                tracing::debug!(%error, "browser session handshake failed");
                return None;
            }
            Err(HandshakeError::Interrupted(midway)) => {
                if handle.close.requested().is_some() || Instant::now() >= deadline {
                    tracing::debug!("browser session handshake abandoned");
                    return None;
                }
                attempt = midway.handshake();
            }
        }
    }
}

/// Как относиться к ошибке чтения.
enum ReadFailure {
    /// За паузу ничего не пришло — это норма.
    Idle,
    /// Соединение исчезло (обрыв, сброс без закрытия).
    PeerGone,
    /// Собеседник нарушил протокол WebSocket (размер, UTF-8, порядок кадров).
    Violation,
}

fn classify_read_error(error: &tungstenite::Error) -> ReadFailure {
    use std::io::ErrorKind;
    match error {
        tungstenite::Error::Io(io_error)
            if matches!(io_error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
        {
            ReadFailure::Idle
        }
        tungstenite::Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)
        | tungstenite::Error::Io(_)
        | tungstenite::Error::ConnectionClosed
        | tungstenite::Error::AlreadyClosed => ReadFailure::PeerGone,
        tungstenite::Error::Protocol(_)
        | tungstenite::Error::Capacity(_)
        | tungstenite::Error::Utf8(_)
        | tungstenite::Error::AttackAttempt => ReadFailure::Violation,
        _ => ReadFailure::PeerGone,
    }
}

/// Что делать циклу дальше.
enum Flow {
    Continue,
    Finished,
}

/// Исходящая очередь или запись сломались: соединение потеряно.
struct LinkBroken;

/// Соединение после рукопожатия.
struct Link<'shared> {
    shared: &'shared Arc<Shared>,
    id: ConnectionId,
    close: Arc<CloseRequest>,
    /// Калитка доставки: закрывается концом сеанса (см. `DeliveryGate`).
    delivery: Arc<DeliveryGate>,
    outgoing: std::sync::mpsc::Receiver<String>,
    socket: WebSocket<TcpStream>,
}

impl Link<'_> {
    // ---- до attach -------------------------------------------------------

    /// Ждёт первое сообщение — `attach` — в пределах срока. `Some` — подключены.
    fn await_attach(&mut self, accepted_at: Instant) -> Option<SessionRef> {
        let deadline = accepted_at + self.shared.policy.attach_timeout;
        loop {
            if let Some(reason) = self.close.requested() {
                self.close_socket(reason);
                return None;
            }
            if Instant::now() >= deadline {
                self.close_socket(SessionCloseReason::AttachTimeout);
                return None;
            }
            match self.socket.read() {
                Ok(Message::Text(text)) => return self.verify_attach(text.as_str().as_bytes()),
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(Message::Close(_)) => {
                    self.acknowledge_peer_close();
                    return None;
                }
                // Двоичное первым сообщением, сырой кадр — нарушение.
                Ok(Message::Binary(_) | Message::Frame(_)) => {
                    self.close_socket(SessionCloseReason::ProtocolViolation);
                    return None;
                }
                Err(error) => match classify_read_error(&error) {
                    ReadFailure::Idle => {}
                    ReadFailure::PeerGone => return None,
                    ReadFailure::Violation => {
                        self.close_after_read_error(SessionCloseReason::ProtocolViolation);
                        return None;
                    }
                },
            }
        }
    }

    /// Проверяет первое сообщение; при успехе отвечает `attached`.
    fn verify_attach(&mut self, bytes: &[u8]) -> Option<SessionRef> {
        let (session, token) = match decode_message::<PageSessionMessage>(bytes) {
            Ok(PageSessionMessage::Attach { session_key, token }) => (session_key, token),
            Ok(_) | Err(_) => {
                self.close_socket(SessionCloseReason::ProtocolViolation);
                return None;
            }
        };
        let attached = match self.shared.attach(self.id, &session, &token) {
            Ok(attached) => attached,
            Err(rejection) => {
                tracing::warn!(?rejection, "browser session attach rejected");
                self.close_socket(rejection.close_reason());
                return None;
            }
        };
        // Пропуск предъявлен: дальше нужны крупные рамки протокола, а не лимит attach.
        self.socket.set_config(|config| {
            config.max_message_size = Some(MAX_WEBSOCKET_MESSAGE_BYTES);
            config.max_frame_size = Some(MAX_WEBSOCKET_MESSAGE_BYTES);
        });
        if self
            .send_message(&PlayerSessionMessage::Attached {})
            .is_err()
        {
            self.shared.connection_lost(self.id);
            return None;
        }
        Some(attached)
    }

    // ---- после attach ----------------------------------------------------

    /// Основной цикл подключённого соединения.
    fn serve_attached(&mut self, session: &SessionRef) {
        let mut gate = StreamGate::new();
        loop {
            if let Some(reason) = self.close.requested() {
                // Сеанс уже закончил тот, кто попросил (замена, выход, команда плеера).
                self.close_socket(reason);
                return;
            }
            if self.flush_outgoing().is_err() {
                self.shared.connection_lost(self.id);
                return;
            }
            let flow = match self.socket.read() {
                Ok(Message::Text(text)) => {
                    self.on_text(session, &mut gate, text.as_str().as_bytes())
                }
                Ok(Message::Binary(bytes)) => self.on_binary(session, &mut gate, &bytes),
                Ok(Message::Ping(_) | Message::Pong(_)) => Flow::Continue,
                Ok(Message::Close(frame)) => {
                    let code = frame.map(|close| u16::from(close.code));
                    self.acknowledge_peer_close();
                    self.shared.session_closed_by_page(self.id, code);
                    Flow::Finished
                }
                Ok(Message::Frame(_)) => self.violate(),
                Err(error) => match classify_read_error(&error) {
                    ReadFailure::Idle => Flow::Continue,
                    ReadFailure::PeerGone => {
                        self.shared.connection_lost(self.id);
                        Flow::Finished
                    }
                    ReadFailure::Violation => self.violate_after_read_error(),
                },
            };
            if matches!(flow, Flow::Finished) {
                return;
            }
        }
    }

    fn on_text(&mut self, session: &SessionRef, gate: &mut StreamGate, bytes: &[u8]) -> Flow {
        if self.close.requested().is_some() {
            return Flow::Continue; // сеанс уже закончен снаружи, данные не нужны
        }
        let message = match decode_message::<PageSessionMessage>(bytes) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "browser session sent a malformed message");
                return self.violate();
            }
        };
        match &message {
            // Вторая attach в уже подключённом соединении.
            PageSessionMessage::Attach { .. } => return self.violate(),
            PageSessionMessage::TabGone { .. } => return self.finish_tab_gone(session, message),
            _ => {}
        }
        match gate.apply_message(&message) {
            Ok(GateDecision::Accept) => {
                self.publish_page_message(session, message);
                Flow::Continue
            }
            Ok(GateDecision::DropStale) => {
                self.shared.count_stale_message();
                Flow::Continue
            }
            Err(violation) => {
                tracing::warn!(%violation, "browser session stream violation");
                self.violate()
            }
        }
    }

    fn on_binary(&mut self, session: &SessionRef, gate: &mut StreamGate, bytes: &[u8]) -> Flow {
        if self.close.requested().is_some() {
            return Flow::Continue;
        }
        let frame = match decode_data_frame(bytes) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "browser session sent a malformed data frame");
                return self.violate();
            }
        };
        let source = match frame.header {
            DataFrameHeader::SourceBufferBytes(header) => {
                match gate.admit_source_buffer_bytes(&header) {
                    Ok(GateDecision::Accept) => ChunkSource::SourceBuffer(SourceBufferChunk {
                        generation: header.generation,
                        source_buffer: header.source_buffer,
                        sequence: header.sequence,
                        payload: frame.payload,
                    }),
                    Ok(GateDecision::DropStale) => {
                        self.shared.count_stale_frame();
                        return Flow::Continue;
                    }
                    Err(violation) => {
                        tracing::warn!(%violation, "browser session stream violation");
                        return self.violate();
                    }
                }
            }
            // Рамки `<video src>` сторож не охватывает: передаём как есть (см. RangeChunk).
            DataFrameHeader::RangeBytes(header) => ChunkSource::Range(RangeChunk {
                request_id: header.request_id,
                offset: header.offset.as_bytes(),
                payload: frame.payload,
            }),
        };
        // Через калитку: если сеанс закончился между проверкой и выдачей, байты
        // не уйдут приёмнику после его `session_ended`.
        let delivered = self.delivery.deliver(|| {
            self.shared
                .byte_sink
                .accept(SessionChunk { session, source });
        });
        if delivered {
            self.shared.count_delivered_chunk();
        }
        Flow::Continue
    }

    /// `tab_gone`: сообщение приёмнику, затем закрытие кодом 4002.
    fn finish_tab_gone(&mut self, session: &SessionRef, message: PageSessionMessage) -> Flow {
        self.publish_page_message(session, message);
        self.shared
            .session_closed_by_player(self.id, SessionCloseReason::TabGone);
        self.close_socket(SessionCloseReason::TabGone);
        Flow::Finished
    }

    /// Нарушение протокола: сеанс закончен, соединение закрыто кодом 4003.
    fn violate(&mut self) -> Flow {
        self.shared
            .session_closed_by_player(self.id, SessionCloseReason::ProtocolViolation);
        self.close_socket(SessionCloseReason::ProtocolViolation);
        Flow::Finished
    }

    /// То же нарушение, но найденное самим чтением сокета (размер, UTF-8, порядок
    /// кадров): внутреннее состояние чтения библиотеки после такой ошибки
    /// недействительно, поэтому ответа собеседника ждём прямым чтением TCP.
    fn violate_after_read_error(&mut self) -> Flow {
        self.shared
            .session_closed_by_player(self.id, SessionCloseReason::ProtocolViolation);
        self.close_after_read_error(SessionCloseReason::ProtocolViolation);
        Flow::Finished
    }

    fn publish_page_message(&self, session: &SessionRef, message: PageSessionMessage) {
        let mut sink_result = Ok(());
        let delivered = self.delivery.deliver(|| {
            sink_result = self.shared.events.publish_page_message(session, message);
        });
        if delivered && sink_result.is_err() {
            let dropped_total = self.shared.count_dropped_page_message();
            // Страница сама решает, сколько сообщений слать: журнал не должен
            // зависеть от её усердия. Пишем первую потерю и каждую сотую,
            // точный счёт остаётся в отчёте `shutdown`.
            if should_log_dropped_message(dropped_total) {
                tracing::warn!(
                    session = session.key.as_str(),
                    dropped_total,
                    "session event sink is full; page messages are being dropped"
                );
            }
        }
    }

    // ---- запись и закрытие -----------------------------------------------

    fn send_message(&mut self, message: &PlayerSessionMessage) -> Result<(), LinkBroken> {
        let json = encode_message(message).map_err(|error| {
            tracing::error!(%error, "player message does not pass protocol validation");
            LinkBroken
        })?;
        self.send_json(json)
    }

    fn send_json(&mut self, json: String) -> Result<(), LinkBroken> {
        self.socket.send(Message::text(json)).map_err(|error| {
            tracing::debug!(%error, "browser session write failed");
            LinkBroken
        })
    }

    /// Отправляет всё, что плеер поставил в очередь.
    fn flush_outgoing(&mut self) -> Result<(), LinkBroken> {
        while let Ok(json) = self.outgoing.try_recv() {
            self.send_json(json)?;
        }
        Ok(())
    }

    /// Закрывает WebSocket кодом `reason` (если уже просили иной — тем) и
    /// ждёт ответный кадр не дольше `close_linger`, чтобы получатель точно
    /// увидел код (иначе RST при закрытии TCP может съесть кадр).
    fn close_socket(&mut self, reason: SessionCloseReason) {
        if !self.send_close_frame(reason) {
            return;
        }
        let deadline = Instant::now() + self.shared.policy.close_linger;
        while Instant::now() < deadline {
            match self.socket.read() {
                Ok(_) => {}
                Err(error) => match classify_read_error(&error) {
                    ReadFailure::Idle => {}
                    ReadFailure::PeerGone | ReadFailure::Violation => return,
                },
            }
        }
    }

    /// Закрытие после ошибки чтения: кадр закрытия отправляем, а входящее
    /// вычитываем сырым TCP (библиотечное чтение после своей ошибки не вызываем).
    /// Вычитывание нужно, чтобы закрытие сокета с непрочитанными данными не
    /// превратилось в RST, который съест наш кадр у собеседника.
    fn close_after_read_error(&mut self, reason: SessionCloseReason) {
        if !self.send_close_frame(reason) {
            return;
        }
        let deadline = Instant::now() + self.shared.policy.close_linger;
        let mut scratch = [0_u8; 8192];
        while Instant::now() < deadline {
            match self.socket.get_mut().read(&mut scratch) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    }

    /// Ставит в сокет кадр закрытия; `false` — доставить не удалось.
    fn send_close_frame(&mut self, reason: SessionCloseReason) -> bool {
        let reason = self.close.requested().unwrap_or(reason);
        let frame = CloseFrame {
            code: CloseCode::from(reason.close_code()),
            reason: "".into(),
        };
        match self.socket.close(Some(frame)) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%error, "browser session close frame was not delivered");
                false
            }
        }
    }

    /// Страница прислала кадр закрытия: библиотека уже поставила ответный в очередь,
    /// дописываем его в сокет.
    fn acknowledge_peer_close(&mut self) {
        if let Err(error) = self.socket.flush() {
            tracing::debug!(%error, "close acknowledgement was not flushed");
        }
    }
}

#[cfg(test)]
mod tests;
