//! Сайт стенда: маленький HTTP-сервер на `std::net`, отдающий страницы, медиа и принимающий отчёты.
//!
//! Два слушателя на одном наборе маршрутов: «главный» origin `http://127.0.0.1:<порт>` и
//! «чужой» `http://localhost:<порт2>`. Оба слушают 127.0.0.1, но браузер различает их по
//! имени хоста — а значит, по origin и по кукам; на этом строятся сценарии `iframe-cross-origin`
//! и `cdn-guard`.
//!
//! Кто чем владеет: `reports` — отчёты и журнал, `cdn_guard` — куки/Referer и счётчики,
//! `static_files` — Content-Type и Range, `routes` — таблица маршрутов, `server` — сокеты и
//! потоки, `http` — разбор и запись HTTP, `hls_local_segments` — HLS-плейлист с сегментами на
//! локальном адресе (сценарий `local-address`). Снаружи используется только [`StandSite`].

mod cdn_guard;
mod error;
mod hls_local_segments;
mod http;
mod reports;
mod request_log;
mod routes;
mod run_id;
mod server;
mod static_files;

pub use cdn_guard::CdnRejections;
pub use error::SiteError;
pub use reports::{BrowserLogLine, LogSource, PageReport, PageStatus, ReportCheck};
pub use request_log::{MediaRequest, MediaRequester, PAGE_FETCH_MARKER_HEADER, ServedSpan};
pub use run_id::RunId;

use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::media::MediaCatalog;

use routes::{RouteContext, SharedState};
use server::Listener;

/// Имя хоста «чужого» origin. Разрешается в 127.0.0.1 (а на некоторых системах ещё и в ::1).
const OTHER_ORIGIN_HOST: &str = "localhost";
/// Адрес привязки обоих слушателей: только петля, наружу стенд не торчит.
const BIND_ADDRESS: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// Параметры запуска.
pub struct StandSiteConfig {
    /// Каталог со статикой страниц (`crates/browser-bridge-bench/site`).
    pub site_root: PathBuf,
    /// Медиа, отдаваемое по `/media/…` и `/cdn/media/…`.
    pub media: MediaCatalog,
}

/// Работающий сайт. Останавливается при `Drop` (потоки приёма завершаются, порты освобождаются).
pub struct StandSite {
    main_origin: String,
    other_origin: String,
    shared: Arc<SharedState>,
    // Слушатели остановятся в Drop этих полей.
    _main_listener: Listener,
    _other_listener: Listener,
}

impl StandSite {
    /// Занимает два порта (выбирает ОС) и начинает обслуживание.
    pub fn start(config: StandSiteConfig) -> Result<Self, SiteError> {
        let main_socket = bind_loopback("main")?;
        let other_socket = bind_loopback("other")?;
        let main_origin = format!("http://127.0.0.1:{}", local_port(&main_socket, "main")?);
        let other_origin = format!(
            "http://{OTHER_ORIGIN_HOST}:{}",
            local_port(&other_socket, "other")?
        );
        let shared = Arc::new(SharedState {
            site_root: config.site_root,
            media: config.media,
            reports: reports::ReportStore::default(),
            cdn: cdn_guard::CdnGuard::default(),
            requests: request_log::RequestLog::default(),
            recording: std::sync::Mutex::new(None),
        });
        let main_listener = start_listener(main_socket, &shared, &main_origin)?;
        let other_listener = start_listener(other_socket, &shared, &other_origin)?;
        Ok(Self {
            main_origin,
            other_origin,
            shared,
            _main_listener: main_listener,
            _other_listener: other_listener,
        })
    }

    /// Главный origin, например `http://127.0.0.1:41234` (без завершающего `/`).
    pub fn main_origin(&self) -> String {
        self.main_origin.clone()
    }

    /// Чужой origin, например `http://localhost:41235` (без завершающего `/`).
    pub fn other_origin(&self) -> String {
        self.other_origin.clone()
    }

    /// Ждёт отчёт страницы с данным `run`. Повторный вызов вернёт тот же отчёт.
    pub fn wait_report(&self, run: &RunId, timeout: Duration) -> Result<PageReport, SiteError> {
        self.shared.reports.wait_report(run, timeout)
    }

    /// Ждёт строку журнала страницы, содержащую `needle` (журнал при этом не расходуется).
    pub fn wait_log_line(
        &self,
        run: &RunId,
        needle: &str,
        timeout: Duration,
    ) -> Result<String, SiteError> {
        self.shared.reports.wait_log_line(run, needle, timeout)
    }

    /// Все строки журнала страницы с `needle` по порядку прихода (журнал не расходуется).
    /// Нужен, когда важна именно НОВАЯ строка: робот считает строки до действия и ждёт рост числа.
    pub fn log_lines_containing(&self, run: &RunId, needle: &str) -> Vec<String> {
        self.shared.reports.lines_containing(run, needle)
    }

    /// Отправляет странице команду; страница заберёт её при очередном опросе.
    pub fn send_command(&self, run: &RunId, command: &str) {
        self.shared.reports.push_command(run, command);
    }

    /// Забирает накопленные строки журнала браузера для прогона (в порядке прихода).
    pub fn take_logs(&self, run: &RunId) -> Vec<BrowserLogLine> {
        self.shared.reports.take_logs(run)
    }

    /// Сколько запросов `/cdn/media/…` отклонено и по какой причине.
    pub fn cdn_rejections(&self) -> CdnRejections {
        self.shared.cdn.rejections()
    }

    /// Запросы медиа (`/media/…`, `/cdn/media/…`) по порядку прихода, с заголовками.
    pub fn media_requests(&self) -> Vec<MediaRequest> {
        self.shared.requests.snapshot()
    }

    /// Выбирает файл записи потока страницы: его отдаёт `GET /bench/recording` (команда `replay`).
    pub fn serve_recording(&self, path: PathBuf) {
        let mut selected = match self.shared.recording.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *selected = Some(path);
    }

    /// Сколько запросов `/cdn/media/…` обслужено.
    pub fn cdn_served(&self) -> u64 {
        self.shared.cdn.served()
    }
}

fn bind_loopback(role: &'static str) -> Result<TcpListener, SiteError> {
    TcpListener::bind((BIND_ADDRESS, 0)).map_err(|source| SiteError::Bind {
        role,
        address: format!("{BIND_ADDRESS}:0"),
        source,
    })
}

fn local_port(socket: &TcpListener, role: &'static str) -> Result<u16, SiteError> {
    socket
        .local_addr()
        .map(|address| address.port())
        .map_err(|source| SiteError::Bind {
            role,
            address: "local address".to_owned(),
            source,
        })
}

fn start_listener(
    socket: TcpListener,
    shared: &Arc<SharedState>,
    own_origin: &str,
) -> Result<Listener, SiteError> {
    let context = RouteContext {
        shared: Arc::clone(shared),
        own_origin: own_origin.to_owned(),
    };
    Listener::spawn(socket, context).map_err(|source| SiteError::Io {
        action: "start a listener thread",
        source,
    })
}

#[cfg(test)]
mod tests;
