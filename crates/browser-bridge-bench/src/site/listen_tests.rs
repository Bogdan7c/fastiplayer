//! Тесты выбора адреса главного origin ([`MainListenAddress`]): автоматика — петля, ручной режим —
//! ровно тот адрес и порт, что попросил человек, «любой» адрес и занятый порт — честная ошибка.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};

use super::*;

/// Пустой сайт стенда (каталог без медиа: для проверки слушателей содержимое не нужно).
fn config(dir: &tempfile::TempDir) -> StandSiteConfig {
    let media_root = dir.path().join("media");
    std::fs::create_dir_all(&media_root).expect("media dir");
    std::fs::write(
        media_root.join("catalog.json"),
        r#"{"version":1,"assets":{}}"#,
    )
    .expect("catalog");
    let site_root = dir.path().join("site");
    std::fs::create_dir_all(&site_root).expect("site dir");
    StandSiteConfig {
        site_root,
        media: crate::media::MediaCatalog::load(&media_root).expect("catalog loads"),
    }
}

/// Статус ответа сайта по origin на неизвестный путь (наш сервер отвечает 404).
fn status_of(origin: &str) -> u16 {
    let host_port = origin.strip_prefix("http://").expect("http origin");
    let mut stream = TcpStream::connect(host_port).expect("connect");
    write!(
        stream,
        "GET /bench/no-such-route HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n"
    )
    .expect("send");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read");
    raw.split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status code")
}

#[test]
fn the_automation_listens_on_loopback_with_a_port_chosen_by_the_system() {
    let dir = tempfile::tempdir().expect("temp dir");
    let site = StandSite::start(config(&dir)).expect("site starts");
    let origin = site.main_origin();
    assert!(origin.starts_with("http://127.0.0.1:"), "{origin}");
    assert!(
        !origin.ends_with(":0"),
        "the real port is in the origin: {origin}"
    );
    // Сервер отвечает по этому origin (404 на неизвестный путь — значит, это наш сайт).
    assert_eq!(status_of(&origin), 404);
}

#[test]
fn an_exact_address_is_used_for_the_main_origin_and_the_other_stays_on_loopback() {
    let dir = tempfile::tempdir().expect("temp dir");
    // 127.0.0.2 — тоже петля Linux, но не тот адрес, что зашит по умолчанию: origin обязан его взять.
    let requested = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 2), 0);
    let site = StandSite::start_listening(config(&dir), MainListenAddress::Exact(requested))
        .expect("site starts");
    let origin = site.main_origin();
    assert!(origin.starts_with("http://127.0.0.2:"), "{origin}");
    assert_eq!(status_of(&origin), 404);
    assert!(
        site.other_origin().starts_with("http://localhost:"),
        "{}",
        site.other_origin()
    );
}

#[test]
fn an_exact_port_is_kept_in_the_origin() {
    let dir = tempfile::tempdir().expect("temp dir");
    // Свободный порт: занять и сразу отпустить (ОС его не выдаст другому мгновенно).
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|probe| probe.local_addr())
        .expect("free port")
        .port();
    let site = StandSite::start_listening(
        config(&dir),
        MainListenAddress::Exact(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)),
    )
    .expect("site starts");
    assert_eq!(site.main_origin(), format!("http://127.0.0.1:{port}"));
}

#[test]
fn the_unspecified_address_is_refused_because_no_origin_can_be_built_from_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let result = StandSite::start_listening(
        config(&dir),
        MainListenAddress::Exact(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
    );
    match result {
        Err(SiteError::Bind { role, source, .. }) => {
            assert_eq!(role, "main");
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput);
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(site) => panic!("0.0.0.0 was accepted: {}", site.main_origin()),
    }
}

#[test]
fn a_busy_port_is_an_honest_bind_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("occupy a port");
    let address = match busy.local_addr().expect("address") {
        std::net::SocketAddr::V4(address) => address,
        std::net::SocketAddr::V6(_) => panic!("loopback v4 expected"),
    };
    let result = StandSite::start_listening(config(&dir), MainListenAddress::Exact(address));
    assert!(
        matches!(result, Err(SiteError::Bind { role: "main", .. })),
        "{:?}",
        result.err()
    );
}
