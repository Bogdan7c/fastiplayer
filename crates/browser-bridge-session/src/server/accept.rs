//! Цикл приёма: принимает TCP-соединения и ведёт часы сроков реестра.
//!
//! Один поток на весь сервер. Слушающий сокет неблокирующий: цикл просыпается
//! раз в `connection_poll_interval`, принимает всё накопившееся, применяет
//! истёкшие сроки и проверяет флаг выключения. Так не нужен ни tokio, ни
//! «самоподключение» для остановки блокирующего `accept`.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Instant;

use super::connection;
use super::shared::{AdmissionRefusal, Shared};

/// Тело потока приёма.
pub(super) fn run(shared: &Arc<Shared>, listener: &TcpListener) {
    let poll_interval = shared.policy.connection_poll_interval;
    while !shared.is_stopping() {
        shared.expire_deadlines(Instant::now());
        if !accept_pending(shared, listener) {
            std::thread::sleep(poll_interval);
        }
    }
}

/// Принимает одно ожидающее соединение; `true` — оно было (звать ещё раз без паузы).
fn accept_pending(shared: &Arc<Shared>, listener: &TcpListener) -> bool {
    match listener.accept() {
        Ok((stream, peer)) => {
            tracing::debug!(%peer, "browser session tcp connection accepted");
            admit(shared, stream);
            true
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => false,
        Err(error) => {
            // Сбой одного приёма (например, оборванное до accept подключение) не
            // повод ронять сервер; пауза не даёт крутить цикл вхолостую.
            tracing::warn!(%error, "browser session accept failed");
            false
        }
    }
}

/// Решает судьбу соединения: поток или немедленный сброс.
fn admit(shared: &Arc<Shared>, stream: std::net::TcpStream) {
    // Принятый сокет может унаследовать неблокирующий режим (BSD, Windows).
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::warn!(%error, "cannot switch accepted socket to blocking mode");
        return;
    }
    let handle = match shared.register_connection() {
        Ok(handle) => handle,
        Err(AdmissionRefusal::TooManyUnattached) => {
            tracing::warn!("too many unattached browser session connections; dropping a new one");
            return;
        }
        Err(AdmissionRefusal::ShuttingDown) => return,
    };
    let connection_id = handle.id;
    let worker_shared = Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name("browser-session-conn".to_owned())
        .spawn(move || connection::run(&worker_shared, stream, handle));
    match spawned {
        Ok(worker) => {
            let mut workers = shared
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            workers.retain(|existing| !existing.is_finished());
            workers.push(worker);
        }
        Err(error) => {
            tracing::error!(%error, "cannot spawn a browser session connection thread");
            shared.release_connection(connection_id);
        }
    }
}
