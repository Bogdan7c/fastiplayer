//! Слушатель TCP: принимает соединения, по потоку на каждое, останавливается по флагу.
//!
//! Способ остановки: сокет слушателя неблокирующий, цикл раз в [`ACCEPT_POLL_INTERVAL`]
//! проверяет флаг. Альтернатива — «разбудить» блокирующий `accept` соединением к самому себе —
//! сложнее и ломается, если порт успел занять кто-то другой; опрос раз в 10 мс стоит
//! пренебрежимо мало на тестовом стенде и не зависит от платформы.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::http::{RequestError, Response, read_request, write_response};
use super::routes::{RouteContext, route};

/// Период опроса флага остановки, когда новых соединений нет.
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Сколько соединений обслуживаем одновременно; сверх лимита — 503, а не бесконечный рост потоков.
const MAX_CONCURRENT_CONNECTIONS: usize = 64;
/// Клиент, который молчит дольше, отваливается (иначе зависшее соединение держит поток).
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Запись ответа большим клиентам-«тормозам» ограничена, но щедро: сегменты — мегабайты.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Запущенный слушатель: поток приёма и флаг остановки.
pub(super) struct Listener {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    /// Запускает поток приёма для уже занятого сокета.
    pub(super) fn spawn(listener: TcpListener, context: RouteContext) -> std::io::Result<Self> {
        listener.set_nonblocking(true)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread = thread::Builder::new()
            .name("bench-site-accept".to_owned())
            .spawn(move || accept_loop(&listener, &Arc::new(context), &thread_shutdown))?;
        Ok(Self {
            shutdown,
            thread: Some(thread),
        })
    }

    /// Останавливает приём и ждёт завершения потока (сокет закрывается вместе с ним).
    pub(super) fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            // Паника в потоке приёма уже напечатана рантаймом; тут важно лишь дождаться выхода.
            let _ = thread.join();
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: &TcpListener, context: &Arc<RouteContext>, shutdown: &AtomicBool) {
    let active_connections = Arc::new(AtomicUsize::new(0));
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _peer)) => dispatch(stream, context, &active_connections),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL)
            }
            Err(error) => {
                // Временные сбои accept (например, нехватка дескрипторов) не должны крутить цикл вхолостую.
                eprintln!("bench site: accept failed: {error}");
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

/// Счётчик активных соединений: уменьшается сам при выходе из потока, даже при панике.
struct ConnectionSlot(Arc<AtomicUsize>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn dispatch(
    mut stream: TcpStream,
    context: &Arc<RouteContext>,
    active_connections: &Arc<AtomicUsize>,
) {
    if active_connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CONNECTIONS {
        active_connections.fetch_sub(1, Ordering::AcqRel);
        reject_overloaded(&mut stream);
        return;
    }
    let slot = ConnectionSlot(Arc::clone(active_connections));
    let context = Arc::clone(context);
    let spawned = thread::Builder::new()
        .name("bench-site-conn".to_owned())
        .spawn(move || {
            let _slot = slot;
            handle_connection(stream, &context);
        });
    if let Err(error) = spawned {
        // Поток не создался: слот освободился вместе с закрытой замыканием структурой.
        eprintln!("bench site: cannot spawn a connection thread: {error}");
    }
}

fn reject_overloaded(stream: &mut TcpStream) {
    // Ошибка записи здесь не важна: клиент и так получает отказ.
    let _ = write_response(stream, Response::text(503, "too many connections"), true);
}

fn handle_connection(mut stream: TcpStream, context: &RouteContext) {
    // Принятый сокет мог унаследовать «неблокирующий» режим слушателя — нам нужен блокирующий с таймаутами.
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(READ_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
    {
        return;
    }
    let (response, include_body) = match read_request(&mut stream) {
        Ok(request) => {
            let include_body = request.method != "HEAD";
            (route(context, &request), include_body)
        }
        Err(RequestError::Disconnected) => return,
        Err(error) => (error_response(&error), true),
    };
    // Клиент мог закрыть соединение раньше (браузер отменил загрузку) — это не ошибка стенда.
    let _ = write_response(&mut stream, response, include_body);
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn error_response(error: &RequestError) -> Response {
    match error {
        RequestError::Malformed(reason) => Response::text(400, reason),
        RequestError::HeadTooLarge => Response::text(431, "request head too large"),
        RequestError::BodyTooLarge => Response::text(413, "request body too large"),
        RequestError::Unsupported(what) => Response::text(501, &format!("{what} is not supported")),
        RequestError::Disconnected => Response::new(400),
    }
}
