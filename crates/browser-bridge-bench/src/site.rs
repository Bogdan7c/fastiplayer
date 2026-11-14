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
//! локальном адресе (сценарий `local-address`), `hls_live` — «живой эфир» HLS из VOD-ролика по часам,
//! `hls_live_ends` — такой же эфир, который кончается `ENDLIST`, `hls_live_sleep` — эфир, сервер
//! которого на время «засыпает» (503 на плейлист, окно уезжает дальше).
//! Снаружи используется только [`StandSite`].

mod cdn_guard;
mod error;
mod hls_live;
mod hls_live_ends;
mod hls_live_sleep;
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
pub use hls_live::{
    INITIAL_WINDOW_SEGMENTS as HLS_LIVE_INITIAL_WINDOW_SEGMENTS,
    PLAYLIST_NAME as HLS_LIVE_PLAYLIST_NAME, ROUTE_PREFIX as HLS_LIVE_ROUTE_PREFIX,
    WINDOW_SEGMENTS as HLS_LIVE_WINDOW_SEGMENTS,
};
pub use hls_live_ends::{
    ENDS_AFTER_PUBLISHED_SEGMENTS as HLS_LIVE_ENDS_AFTER_PUBLISHED_SEGMENTS,
    ROUTE_PREFIX as HLS_LIVE_ENDS_ROUTE_PREFIX,
};
pub use hls_live_sleep::{
    ASLEEP_STATUS as HLS_LIVE_SLEEP_ASLEEP_STATUS, ROUTE_PREFIX as HLS_LIVE_SLEEP_ROUTE_PREFIX,
    SLEEP_SCHEDULE as HLS_LIVE_SLEEP_SCHEDULE,
    SOURCE_SEGMENT_PERIOD as HLS_LIVE_SLEEP_SEGMENT_PERIOD, SleepSchedule,
    first_segment_unseen_before_sleep,
};
pub use reports::{BrowserLogLine, LogSource, PageReport, PageStatus, ReportCheck};
pub use request_log::{MediaRequest, MediaRequester, PAGE_FETCH_MARKER_HEADER, ServedSpan};
pub use run_id::RunId;

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::media::MediaCatalog;

use routes::{RouteContext, SharedState};
use server::Listener;

/// Имя хоста «чужого» origin. Разрешается в 127.0.0.1 (а на некоторых системах ещё и в ::1).
const OTHER_ORIGIN_HOST: &str = "localhost";
/// Адрес привязки обоих слушателей по умолчанию: только петля, наружу стенд не торчит.
const BIND_ADDRESS: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// Где слушает ГЛАВНЫЙ origin сайта.
///
/// Автоматика стенда всегда слушает петлю ([`StandSite::start`]). Адрес в локальной сети — только
/// явный выбор человека для ручных проверок, где браузер живёт на ДРУГОЙ машине (16d4: стенд на
/// основном ПК не спит, пока спит ноутбук с браузером). «Чужой» origin (`localhost`) при этом
/// остаётся на петле: с другой машины он недоступен, и страницы, которым он нужен
/// (`iframe-cross-origin`, `cdn-guard`), так не проверяются.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainListenAddress {
    /// `127.0.0.1`, порт выбирает ОС (как у автоматики стенда).
    Loopback,
    /// Конкретный адрес и порт (порт 0 — выбирает ОС). Адрес «любой» (`0.0.0.0`) не годится:
    /// из него не построить origin, по которому браузер придёт на сайт.
    Exact(SocketAddrV4),
}

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
    /// Занимает два порта на петле (выбирает ОС) и начинает обслуживание.
    pub fn start(config: StandSiteConfig) -> Result<Self, SiteError> {
        Self::start_listening(config, MainListenAddress::Loopback)
    }

    /// Как [`Self::start`], но главный origin слушает `main_address` (см. [`MainListenAddress`]).
    pub fn start_listening(
        config: StandSiteConfig,
        main_address: MainListenAddress,
    ) -> Result<Self, SiteError> {
        let main_bind = match main_address {
            MainListenAddress::Loopback => SocketAddrV4::new(BIND_ADDRESS, 0),
            MainListenAddress::Exact(address) => address,
        };
        let main_socket = bind_on(main_bind, "main")?;
        let other_socket = bind_on(SocketAddrV4::new(BIND_ADDRESS, 0), "other")?;
        let main_origin = format!(
            "http://{}:{}",
            main_bind.ip(),
            local_port(&main_socket, "main")?
        );
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
            hls_live: hls_live::LiveClocks::default(),
            hls_live_ends: hls_live::LiveClocks::default(),
            hls_live_sleep: hls_live::LiveClocks::default(),
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

/// Занимает порт слушателя `role` на `address`; «любой» адрес отвергается (см. [`MainListenAddress`]).
fn bind_on(address: SocketAddrV4, role: &'static str) -> Result<TcpListener, SiteError> {
    let bind_error = |source| SiteError::Bind {
        role,
        address: address.to_string(),
        source,
    };
    if address.ip().is_unspecified() {
        return Err(bind_error(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the stand needs a concrete address to build its origin, not 0.0.0.0",
        )));
    }
    TcpListener::bind(address).map_err(bind_error)
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

#[cfg(test)]
mod listen_tests;
