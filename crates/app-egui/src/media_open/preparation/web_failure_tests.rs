//! Сквозные тесты UX сессии 08: настоящий HTTP-сервер → production подготовка web-ссылки
//! → вид ошибки подготовки → текст, который увидит пользователь (окно и строка очереди).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::media_open::{
    MediaOpenSourceRequest, MediaPreparationFailureKind, WebMediaOpenRequest, WebOpenFailureReason,
};
use crate::url_service_adapter::{StartupUrlClassification, classify_startup_url};

/// Ответ loopback-сервера на каждое соединение.
#[derive(Clone, Copy)]
pub(in crate::media_open) enum ServerAnswer {
    /// Строка статуса и дополнительные заголовки, без body.
    Status {
        status_line: &'static str,
        extra_headers: &'static str,
    },
    /// Закрыть соединение, не ответив.
    Close,
}

/// HTTP-сервер на 127.0.0.1 с явной остановкой потока.
pub(in crate::media_open) struct LoopbackServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl LoopbackServer {
    pub(in crate::media_open) fn spawn(answer: ServerAnswer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("local address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join_handle = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => respond(stream, answer),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            join_handle: Some(join_handle),
        }
    }

    /// Ссылка с логином, паролем и токеном: ни один секрет не должен попасть в текст.
    pub(in crate::media_open) fn secret_media_url(&self) -> String {
        format!(
            "http://viewer:hunter2@{}/private/clip.mp4?token=topsecret",
            self.address
        )
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join_handle) = self.join_handle.take() {
            join_handle.join().expect("loopback server thread");
        }
    }
}

/// Дочитывает заголовки запроса и отвечает заданным образом.
fn respond(mut stream: TcpStream, answer: ServerAnswer) {
    stream
        .set_nonblocking(false)
        .expect("blocking accepted stream");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    let mut request_head = Vec::new();
    let mut byte = [0_u8; 1];
    while !request_head.ends_with(b"\r\n\r\n") && request_head.len() < 16 * 1024 {
        match stream.read(&mut byte) {
            Ok(1) => request_head.push(byte[0]),
            _ => break,
        }
    }
    let ServerAnswer::Status {
        status_line,
        extra_headers,
    } = answer
    else {
        return;
    };
    let response = format!(
        "HTTP/1.1 {status_line}\r\n{extra_headers}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    // Клиент мог уже закрыть соединение — для сервера теста это не ошибка.
    let _ignored_write = stream.write_all(response.as_bytes());
}

/// Что увидит пользователь после неудачной подготовки ссылки.
struct UserVisibleFailure {
    kind: MediaPreparationFailureKind,
    window_message: String,
    row_summary: String,
}

/// Production путь: классификация URL → `prepare_source` → тексты окна и строки.
fn open_link_against(answer: ServerAnswer) -> UserVisibleFailure {
    let server = LoopbackServer::spawn(answer);
    let url = server.secret_media_url();
    let StartupUrlClassification::Supported(startup_locator) = classify_startup_url(&url) else {
        panic!("loopback mp4 link should be supported");
    };
    let display_host = startup_locator.display_host();
    let locator = media_source_open::direct_progressive_open::classify_direct_media_url(&url)
        .expect("mp4 loopback link - direct media");
    let request = MediaOpenSourceRequest::Web(WebMediaOpenRequest::direct(
        locator,
        fastiplayer_config::NetworkConfig::default(),
        fastiplayer_config::PlayerDemuxConfig::default(),
    ));
    let cancellation = super::super::executor::PreparationCancellation::new();

    let kind = match super::prepare_source(request, &cancellation) {
        Ok(_) => panic!("A server without media cannot provide a source."),
        Err(kind) => kind,
    };
    let reason = kind
        .user_failure_reason()
        .expect("Web Preparation Failure Must Have a User Reason");
    UserVisibleFailure {
        kind,
        window_message: crate::web_open_message::web_open_failure_message(
            display_host.as_deref(),
            reason,
        ),
        row_summary: crate::local_open_message::local_open_failure_row_summary(reason),
    }
}

/// Текст не раскрывает логин, пароль, путь и query ссылки.
fn assert_no_link_secrets(text: &str) {
    for secret in [
        "viewer",
        "hunter2",
        "private",
        "clip.mp4",
        "token",
        "topsecret",
    ] {
        assert!(
            !text.contains(secret),
            "\"{secret}\" leaked to text: {text}"
        );
    }
}

/// Мёртвая ссылка больше не «нет сети»: окно и строка очереди говорят «не найдена».
#[test]
fn dead_link_shows_not_found_instead_of_no_network() {
    let failure = open_link_against(ServerAnswer::Status {
        status_line: "404 Not Found",
        extra_headers: "",
    });

    assert_eq!(
        failure.kind,
        MediaPreparationFailureKind::DirectOpen(WebOpenFailureReason::NotFound)
    );
    assert_eq!(
        failure.window_message,
        "Could not open the link (127.0.0.1): page not found (error 404); check the link"
    );
    assert_eq!(
        failure.row_summary,
        "Page not found (error 404); check the link"
    );
    assert_no_link_secrets(&failure.window_message);
    assert!(!failure.window_message.contains("joined"));
}

/// 403, 429 с Retry-After и оборванное соединение дают разные понятные тексты.
#[test]
fn forbidden_rate_limited_and_dropped_connection_have_distinct_texts() {
    let forbidden = open_link_against(ServerAnswer::Status {
        status_line: "403 Forbidden",
        extra_headers: "",
    });
    let rate_limited = open_link_against(ServerAnswer::Status {
        status_line: "429 Too Many Requests",
        extra_headers: "Retry-After: 1\r\n",
    });
    let dropped = open_link_against(ServerAnswer::Close);

    assert_eq!(
        forbidden.window_message,
        "Could not open the link (127.0.0.1): the server denied access (error 403)"
    );
    assert_eq!(
        rate_limited.window_message,
        "Could not open the link (127.0.0.1): the server asks you to wait: too many requests; try again later"
    );
    assert_eq!(
        dropped.window_message,
        "Could not open the link (127.0.0.1): cannot connect to the server; check your internet connection"
    );
    for failure in [&forbidden, &rate_limited, &dropped] {
        assert_no_link_secrets(&failure.window_message);
        assert_no_link_secrets(&failure.row_summary);
    }
}
