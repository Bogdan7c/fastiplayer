//! Сервер сеанса: WebSocket на `127.0.0.1` внутри процесса браузерного режима
//! (решения 82 и 85).
//!
//! Устройство (знать его должен только этот модуль, наружу — `SessionControl`):
//! - один поток приёма (`accept`) — принимает TCP и ведёт сроки реестра;
//! - по потоку на соединение (`connection`) — рукопожатие, `attach`, обмен;
//! - общее состояние под одним замком (`shared`) — реестр сеансов, таблица
//!   соединений, события жизни.
//!
//! Tokio нет: хватает std и синхронного `tungstenite`. Сеанс переживает «вход»
//! (D-Bus, посредник): после `open_session` и `attach` байты идут без
//! `SessionControl` (инвариант 3).

mod accept;
mod connection;
mod control;
mod report;
mod shared;

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use browser_bridge_protocol::LoopbackPort;
use browser_bridge_protocol::error::ValueViolation;

pub use control::{CloseOutcome, OpenSessionError, OpenedSession, SendToPageError, SessionControl};
pub use report::ServerShutdownReport;

use self::shared::Shared;
use crate::policy::BrowserSessionPolicy;
use crate::sink::{SessionByteSink, SessionEventSink};
use crate::token::SessionTokenSource;

/// Почему сервер не запустился.
#[derive(Debug, thiserror::Error)]
pub enum ServerStartError {
    /// Не удалось занять порт на loopback.
    #[error("cannot bind the loopback listener: {0}")]
    Bind(std::io::Error),
    /// Не удалось настроить слушающий сокет.
    #[error("cannot configure the loopback listener: {0}")]
    Configure(std::io::Error),
    /// ОС выдала порт, который протокол считает недопустимым.
    #[error("operating system returned an unusable port: {0}")]
    UnusablePort(ValueViolation),
    /// Не удалось запустить поток приёма.
    #[error("cannot spawn the accept thread: {0}")]
    SpawnThread(std::io::Error),
}

/// Запущенный сервер сеанса.
pub struct BrowserSessionServer {
    shared: Arc<Shared>,
    accept_thread: Option<JoinHandle<()>>,
}

impl BrowserSessionServer {
    /// Занимает свободный порт на `127.0.0.1` и запускает приём.
    ///
    /// # Errors
    /// [`ServerStartError`] — порт, сокет или поток не получены.
    pub fn start(
        policy: BrowserSessionPolicy,
        token_source: Box<dyn SessionTokenSource>,
        byte_sink: Arc<dyn SessionByteSink>,
        events: Arc<dyn SessionEventSink>,
    ) -> Result<Self, ServerStartError> {
        // Только loopback и порт 0: ОС выбирает свободный, наружу сеть не торчит.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(ServerStartError::Bind)?;
        listener
            .set_nonblocking(true)
            .map_err(ServerStartError::Configure)?;
        let local_addr = listener.local_addr().map_err(ServerStartError::Configure)?;
        let port = LoopbackPort::new(local_addr.port()).map_err(ServerStartError::UnusablePort)?;
        let shared = Arc::new(Shared::new(policy, port, token_source, byte_sink, events));
        let accept_shared = Arc::clone(&shared);
        let accept_thread = std::thread::Builder::new()
            .name("browser-session-accept".to_owned())
            .spawn(move || accept::run(&accept_shared, &listener))
            .map_err(ServerStartError::SpawnThread)?;
        tracing::info!(
            port = port.get(),
            "browser session server listening on loopback"
        );
        Ok(Self {
            shared,
            accept_thread: Some(accept_thread),
        })
    }

    /// Порт, который надо отдать экстеншену в `session_granted`.
    #[must_use]
    pub fn port(&self) -> LoopbackPort {
        self.shared.port()
    }

    /// Ручка управления для «стойки входа» и приложения.
    #[must_use]
    pub fn control(&self) -> SessionControl {
        SessionControl::new(Arc::clone(&self.shared))
    }

    /// Останавливает сервер: всем соединениям код 4006, приём закрыт, потоки
    /// собраны в пределах `shutdown_join_timeout`. Не виснет.
    pub fn shutdown(mut self) -> ServerShutdownReport {
        self.stop_and_join()
    }

    /// Идемпотентная остановка (её же зовёт `Drop`).
    fn stop_and_join(&mut self) -> ServerShutdownReport {
        self.shared.begin_shutdown();
        if let Some(accept_thread) = self.accept_thread.take()
            && accept_thread.join().is_err()
        {
            tracing::error!("browser session accept thread panicked");
        }
        let detached_threads = self.join_workers();
        self.shared.report(detached_threads)
    }

    /// Ждёт потоки соединений до срока; оставшиеся отпускает и считает.
    fn join_workers(&self) -> usize {
        let deadline = Instant::now() + self.shared.policy.shutdown_join_timeout;
        let mut workers = std::mem::take(
            &mut *self
                .shared
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        while !workers.is_empty() && Instant::now() < deadline {
            let (finished, running): (Vec<_>, Vec<_>) =
                workers.into_iter().partition(JoinHandle::is_finished);
            for worker in finished {
                if worker.join().is_err() {
                    tracing::error!("browser session connection thread panicked");
                }
            }
            workers = running;
            if !workers.is_empty() {
                std::thread::sleep(self.shared.policy.connection_poll_interval);
            }
        }
        if !workers.is_empty() {
            tracing::warn!(
                count = workers.len(),
                "browser session threads did not stop in time"
            );
        }
        workers.len()
    }
}

impl Drop for BrowserSessionServer {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}
