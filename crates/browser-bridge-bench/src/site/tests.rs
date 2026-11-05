//! Тесты сервера по настоящему TCP: поднимаем сайт и ходим на него сырым HTTP-клиентом.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;

use super::*;

/// Детерминированное содержимое «медиафайла»: байт = индекс по модулю 251 (видно любой сдвиг).
fn media_bytes() -> Vec<u8> {
    (0..1000u32).map(|index| (index % 251) as u8).collect()
}

struct Fixture {
    _dir: TempDir,
    site: StandSite,
    secret_path: PathBuf,
}

fn start_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let site_root = dir.path().join("site");
    let media_root = dir.path().join("media");
    fs::create_dir_all(site_root.join("lib")).unwrap();
    fs::create_dir_all(media_root.join("progressive")).unwrap();
    fs::write(site_root.join("index.html"), "<h1>index</h1>").unwrap();
    fs::write(site_root.join("lib/player.js"), "export const x = 1;").unwrap();
    fs::write(media_root.join("progressive/clip.mp4"), media_bytes()).unwrap();
    let secret_path = dir.path().join("secret.txt");
    fs::write(&secret_path, "TOP-SECRET").unwrap();
    fs::write(
        media_root.join("catalog.json"),
        r#"{"version":1,"assets":{"clip":{"kind":"file","url":"progressive/clip.mp4","mime":"video/mp4","duration_s":1.0}}}"#,
    )
    .unwrap();
    let media = crate::media::MediaCatalog::load(&media_root).unwrap();
    let site = StandSite::start(StandSiteConfig { site_root, media }).unwrap();
    Fixture {
        _dir: dir,
        site,
        secret_path,
    }
}

struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// Отправляет запрос на `origin` (вида `http://host:port`) и читает ответ до закрытия соединения.
fn send(
    origin: &str,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> RawResponse {
    let host_port = origin.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(host_port).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {host_port}\r\n");
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> RawResponse {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| line.split_once(": ").unwrap())
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
        .collect();
    RawResponse {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    }
}

fn get(origin: &str, path: &str) -> RawResponse {
    send(origin, "GET", path, &[], &[])
}

#[test]
fn both_origins_serve_the_same_routes_with_different_hosts() {
    let fixture = start_fixture();
    let (main, other) = (fixture.site.main_origin(), fixture.site.other_origin());

    assert!(main.starts_with("http://127.0.0.1:"), "main origin: {main}");
    assert!(
        other.starts_with("http://localhost:"),
        "other origin: {other}"
    );
    assert_ne!(main, other);
    for origin in [&main, &other] {
        let index = get(origin, "/");
        assert_eq!(
            (index.status, index.body.as_slice()),
            (200, b"<h1>index</h1>".as_slice()),
            "origin {origin}"
        );
        assert_eq!(index.headers["content-type"], "text/html; charset=utf-8");
        let script = get(origin, "/site/lib/player.js");
        assert_eq!(script.status, 200);
        assert_eq!(
            script.headers["content-type"],
            "text/javascript; charset=utf-8"
        );
    }
    assert_eq!(get(&main, "/site/missing.js").status, 404);
}

#[test]
fn range_request_returns_206_with_exact_bytes() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();
    let all = media_bytes();

    let partial = send(
        &origin,
        "GET",
        "/media/progressive/clip.mp4",
        &[("Range", "bytes=10-19")],
        &[],
    );
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, all[10..20]);
    assert_eq!(partial.headers["content-range"], "bytes 10-19/1000");
    assert_eq!(partial.headers["content-length"], "10");
    assert_eq!(partial.headers["accept-ranges"], "bytes");
    assert_eq!(partial.headers["access-control-allow-origin"], "*");

    let tail = send(
        &origin,
        "GET",
        "/media/progressive/clip.mp4",
        &[("Range", "bytes=-5")],
        &[],
    );
    assert_eq!((tail.status, tail.body.as_slice()), (206, &all[995..]));

    let open_ended = send(
        &origin,
        "GET",
        "/media/progressive/clip.mp4",
        &[("Range", "bytes=990-")],
        &[],
    );
    assert_eq!(
        (open_ended.status, open_ended.body.as_slice()),
        (206, &all[990..])
    );

    let whole = get(&origin, "/media/progressive/clip.mp4");
    assert_eq!((whole.status, whole.body), (200, all));
}

#[test]
fn bad_range_returns_416_with_total_size() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();

    let beyond_end = send(
        &origin,
        "GET",
        "/media/progressive/clip.mp4",
        &[("Range", "bytes=5000-6000")],
        &[],
    );
    assert_eq!(beyond_end.status, 416);
    assert_eq!(beyond_end.headers["content-range"], "bytes */1000");

    let reversed = send(
        &origin,
        "GET",
        "/media/progressive/clip.mp4",
        &[("Range", "bytes=20-10")],
        &[],
    );
    assert_eq!(reversed.status, 416);
}

#[test]
fn head_request_has_length_but_no_body() {
    let fixture = start_fixture();
    let response = send(
        &fixture.site.main_origin(),
        "HEAD",
        "/media/progressive/clip.mp4",
        &[],
        &[],
    );
    assert_eq!(response.status, 200);
    assert_eq!(response.headers["content-length"], "1000");
    assert!(response.body.is_empty());
}

#[test]
fn media_catalog_is_served_and_parses() {
    let fixture = start_fixture();
    let response = get(&fixture.site.other_origin(), "/media/catalog.json");
    assert_eq!(response.status, 200);
    assert_eq!(response.headers["content-type"], "application/json");
    let parsed: crate::media::CatalogFile = serde_json::from_slice(&response.body).unwrap();
    assert!(parsed.assets.contains_key("clip"));
}

#[test]
fn path_traversal_never_reveals_files_outside_the_roots() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();
    assert!(fixture.secret_path.exists());

    for attack in [
        "/site/../secret.txt",
        "/site/%2e%2e/secret.txt",
        "/site/lib/..%2f..%2fsecret.txt",
        "/media/../secret.txt",
        "/media/%2E%2E%2Fsecret.txt",
        "/media/progressive/../../secret.txt",
        "/cdn/media/../secret.txt",
        "/site/%5c..%5csecret.txt",
        "/site//etc/passwd",
        "/site/%00",
    ] {
        let response = get(&origin, attack);
        assert!(
            matches!(response.status, 400 | 403 | 404),
            "{attack} answered {} instead of a refusal",
            response.status
        );
        assert!(
            !String::from_utf8_lossy(&response.body).contains("TOP-SECRET"),
            "{attack} leaked the secret"
        );
    }
}

#[test]
fn oversized_requests_are_rejected() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();

    let long_header = "x".repeat(20_000);
    let head_too_large = send(
        &origin,
        "GET",
        "/",
        &[("X-Padding", long_header.as_str())],
        &[],
    );
    assert_eq!(head_too_large.status, 431);

    // Тело больше лимита отклоняется по заявленной длине, ещё до чтения.
    let host_port = origin.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(host_port).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"POST /bench/log HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n")
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    assert_eq!(parse_response(&raw).status, 413);
}

#[test]
fn cdn_requires_cookie_and_own_referer() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();
    let other = fixture.site.other_origin();
    let own_referer = format!("{origin}/site/pages/cdn-guard.html");
    let cookie = ("Cookie", "bench_cdn=1");
    let path = "/cdn/media/progressive/clip.mp4";

    // Логин ставит куку на путь /cdn и отвечает 204.
    let login = get(&origin, "/cdn/login");
    assert_eq!(login.status, 204);
    assert_eq!(login.headers["set-cookie"], "bench_cdn=1; Path=/cdn");

    assert_eq!(
        send(
            &origin,
            "GET",
            path,
            &[("Referer", own_referer.as_str())],
            &[]
        )
        .status,
        403,
        "no cookie"
    );
    assert_eq!(
        send(&origin, "GET", path, &[cookie], &[]).status,
        403,
        "no referer"
    );
    assert_eq!(
        send(
            &origin,
            "GET",
            path,
            &[cookie, ("Referer", "http://evil.example/")],
            &[]
        )
        .status,
        403
    );
    // Соседний порт того же хоста — тоже чужой origin (защита от наивного starts_with).
    let lookalike = format!("{origin}9/page");
    assert_eq!(
        send(
            &origin,
            "GET",
            path,
            &[cookie, ("Referer", lookalike.as_str())],
            &[]
        )
        .status,
        403
    );
    // Referer второго origin для первого сервера — чужой.
    let wrong_server_referer = format!("{other}/page");
    assert_eq!(
        send(
            &origin,
            "GET",
            path,
            &[cookie, ("Referer", wrong_server_referer.as_str())],
            &[]
        )
        .status,
        403
    );

    assert_eq!(fixture.site.cdn_served(), 0);
    assert_eq!(
        fixture.site.cdn_rejections(),
        CdnRejections {
            missing_cookie: 1,
            foreign_referer: 4
        }
    );

    let allowed = send(
        &origin,
        "GET",
        path,
        &[cookie, ("Referer", own_referer.as_str())],
        &[],
    );
    assert_eq!((allowed.status, allowed.body), (200, media_bytes()));
    let allowed_range = send(
        &origin,
        "GET",
        path,
        &[cookie, ("Referer", &origin), ("Range", "bytes=0-3")],
        &[],
    );
    assert_eq!(
        (allowed_range.status, allowed_range.body.as_slice()),
        (206, &media_bytes()[..4])
    );
    assert_eq!(fixture.site.cdn_served(), 2);
    assert_eq!(
        fixture.site.cdn_rejections(),
        CdnRejections {
            missing_cookie: 1,
            foreign_referer: 4
        }
    );
}

/// `/cdn/redirect-local/…`: публичный адрес ведёт 302 на файл под настоящим адресом сервера, и шаг
/// перенаправления виден в журнале (сценарий `local-address`).
#[test]
fn redirect_local_answers_302_to_the_main_origin_and_is_logged() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();
    let response = get(&origin, "/cdn/redirect-local/progressive/clip.mp4");
    assert_eq!(response.status, 302);
    assert_eq!(
        response.headers.get("location").map(String::as_str),
        Some(format!("{origin}/media/progressive/clip.mp4").as_str())
    );
    let logged = fixture.site.media_requests();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].path, "/cdn/redirect-local/progressive/clip.mp4");
    assert_eq!(logged[0].status, 302);
}

/// `/cdn/session-media/…` (сессия 09b) проверяет только куку входа: Referer не нужен, без куки — 403.
/// Каждый запрос медиа попадает в журнал сервера с заголовками и статусом.
#[test]
fn cdn_session_media_needs_only_the_cookie_and_every_media_request_is_logged() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();
    let path = "/cdn/session-media/progressive/clip.mp4";
    assert_eq!(
        send(&origin, "GET", path, &[("Range", "bytes=0-3")], &[]).status,
        403,
        "no cookie"
    );
    let allowed = send(
        &origin,
        "GET",
        path,
        &[("Cookie", "bench_cdn=1"), ("Range", "bytes=0-3")],
        &[],
    );
    assert_eq!(
        (allowed.status, allowed.body.as_slice()),
        (206, &media_bytes()[..4])
    );
    // Не медиа — в журнал не попадает (что бы ни ответил сервер).
    let _ = get(&origin, "/site/pages/cdn-guard.html");

    let logged = fixture.site.media_requests();
    assert_eq!(
        logged.len(),
        2,
        "only media requests are logged: {logged:#?}"
    );
    assert_eq!(logged[0].status, 403);
    assert_eq!(logged[1].status, 206);
    assert!(logged[1].has_cookie("bench_cdn"));
    assert!(!logged[0].has_cookie("bench_cdn"));
    assert_eq!(logged[1].range.as_deref(), Some("bytes=0-3"));
    assert_eq!(logged[1].referer, None);
    let span = logged[1].served_span().expect("206 has a Content-Range");
    assert_eq!(
        (span.first, span.end, span.total),
        (0, 4, media_bytes().len() as u64)
    );
    assert_eq!(logged[0].served_span(), None);
}

#[test]
fn posted_report_is_returned_by_wait_report_and_cors_preflight_is_answered() {
    let fixture = start_fixture();
    let run = RunId::new("report-run-1").unwrap();
    let payload = format!(
        r#"{{"run":"{run}","scenario":"mse-fmp4","status":"pass","checks":[{{"name":"frames-advanced","ok":true,"detail":"0 -> 74"}}],"user_agent":"test","elapsed_ms":1234}}"#
    );

    // Отчёт с чужого origin уходит на главный (поэтому нужен CORS на /bench/*).
    let preflight = send(
        &fixture.site.main_origin(),
        "OPTIONS",
        "/bench/report",
        &[("Origin", "http://localhost:1")],
        &[],
    );
    assert_eq!(preflight.status, 204);
    assert_eq!(preflight.headers["access-control-allow-origin"], "*");
    assert_eq!(
        preflight.headers["access-control-allow-methods"],
        "GET, POST, OPTIONS"
    );
    assert_eq!(
        preflight.headers["access-control-allow-headers"],
        "content-type, range"
    );

    let posted = send(
        &fixture.site.main_origin(),
        "POST",
        "/bench/report",
        &[("Content-Type", "application/json")],
        payload.as_bytes(),
    );
    assert_eq!(posted.status, 204);
    assert_eq!(posted.headers["access-control-allow-origin"], "*");

    let report = fixture
        .site
        .wait_report(&run, Duration::from_secs(2))
        .unwrap();
    assert_eq!(report.scenario, "mse-fmp4");
    assert_eq!(report.status, PageStatus::Pass);
    assert_eq!(
        report.checks,
        vec![ReportCheck {
            name: "frames-advanced".into(),
            ok: true,
            detail: "0 -> 74".into()
        }]
    );
    assert_eq!(report.elapsed_ms, 1234);
}

#[test]
fn wait_report_wakes_up_when_report_arrives_later() {
    let fixture = start_fixture();
    let run = RunId::new("late-run").unwrap();
    let origin = fixture.site.main_origin();
    let payload = format!(r#"{{"run":"{run}","scenario":"video-src","status":"unsupported"}}"#);
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        send(&origin, "POST", "/bench/report", &[], payload.as_bytes()).status
    });

    let report = fixture
        .site
        .wait_report(&run, Duration::from_secs(5))
        .unwrap();

    assert_eq!(report.status, PageStatus::Unsupported);
    assert_eq!(sender.join().unwrap(), 204);
}

#[test]
fn wait_report_times_out_with_typed_error() {
    let fixture = start_fixture();
    let run = RunId::new("never-reported").unwrap();

    let error = fixture
        .site
        .wait_report(&run, Duration::from_millis(100))
        .unwrap_err();

    assert!(
        matches!(&error, SiteError::ReportTimeout { run: timed_out, waited } if *timed_out == run && *waited == Duration::from_millis(100)),
        "unexpected error: {error}"
    );
}

#[test]
fn reports_and_logs_are_separated_by_run_and_malformed_input_is_refused() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();
    let first = RunId::new("log-run-a").unwrap();
    let second = RunId::new("log-run-b").unwrap();

    for (run, source, line) in [
        (&first, "extension", "hook installed"),
        (&second, "page", "other run"),
        (&first, "page", "source buffer added"),
    ] {
        let body = format!(r#"{{"source":"{source}","run":"{run}","line":"{line}"}}"#);
        assert_eq!(
            send(&origin, "POST", "/bench/log", &[], body.as_bytes()).status,
            204
        );
    }
    assert_eq!(
        send(&origin, "POST", "/bench/log", &[], b"{broken").status,
        400
    );
    assert_eq!(
        send(
            &origin,
            "POST",
            "/bench/log",
            &[],
            br#"{"source":"page","run":"BAD RUN","line":"x"}"#
        )
        .status,
        400
    );
    assert_eq!(
        send(
            &origin,
            "POST",
            "/bench/report",
            &[],
            br#"{"run":"r","status":"maybe"}"#
        )
        .status,
        400
    );

    let lines: Vec<_> = fixture
        .site
        .take_logs(&first)
        .into_iter()
        .map(|entry| (entry.source, entry.line))
        .collect();
    assert_eq!(
        lines,
        vec![
            (LogSource::Extension, "hook installed".to_owned()),
            (LogSource::Page, "source buffer added".to_owned())
        ]
    );
    // Забранный журнал не возвращается повторно; журнал второго прогона не затронут.
    assert!(fixture.site.take_logs(&first).is_empty());
    assert_eq!(fixture.site.take_logs(&second).len(), 1);
}

#[test]
fn run_id_validation_and_uniqueness() {
    assert!(RunId::new("smoke-1").is_ok());
    for bad in [
        "",
        "UPPER",
        "with space",
        "under_score",
        "юникод",
        &"a".repeat(65),
    ] {
        assert!(
            matches!(RunId::new(bad), Err(SiteError::InvalidRunId(_))),
            "{bad:?} must be refused"
        );
    }
    assert!(RunId::new(&"a".repeat(64)).is_ok());
    let first = RunId::new_unique("Smoke Test!");
    let second = RunId::new_unique("Smoke Test!");
    assert_ne!(first, second);
    assert!(first.as_str().starts_with("smoke-test--"), "got {first}");
    assert!(
        RunId::new(first.as_str()).is_ok(),
        "generated id must itself be valid"
    );
    assert!(RunId::new_unique("").as_str().starts_with("run-"));
}

#[test]
fn dropping_the_site_closes_both_ports() {
    let fixture = start_fixture();
    let addresses: Vec<String> = [fixture.site.main_origin(), fixture.site.other_origin()]
        .iter()
        .map(|origin| origin.strip_prefix("http://").unwrap().to_owned())
        .collect();
    assert!(
        addresses
            .iter()
            .all(|address| TcpStream::connect(address).is_ok())
    );

    drop(fixture);

    for address in &addresses {
        assert!(
            TcpStream::connect(address).is_err(),
            "{address} still accepts connections after drop"
        );
    }
}

#[test]
fn page_polls_robot_commands_in_order_and_sees_only_new_ones() {
    let fixture = start_fixture();
    let run = RunId::new("cmd-run-1").unwrap();
    let other = RunId::new("cmd-run-2").unwrap();
    let origin = fixture.site.main_origin();
    let poll = |run: &RunId, seen: usize| -> Vec<String> {
        let response = get(&origin, &format!("/bench/commands/{run}/{seen}"));
        assert_eq!(response.status, 200);
        assert_eq!(response.headers["access-control-allow-origin"], "*");
        serde_json::from_slice(&response.body).expect("a JSON array of strings")
    };

    assert!(poll(&run, 0).is_empty(), "nothing was sent yet");
    fixture.site.send_command(&run, "handed-off");
    fixture.site.send_command(&run, "expect-paused");
    fixture.site.send_command(&other, "finish");
    assert_eq!(poll(&run, 0), vec!["handed-off", "expect-paused"]);
    // Страница уже получила одну команду — повтора нет.
    assert_eq!(poll(&run, 1), vec!["expect-paused"]);
    assert!(poll(&run, 2).is_empty());
    // Команды чужого прогона не просачиваются.
    assert_eq!(poll(&other, 0), vec!["finish"]);
}

#[test]
fn malformed_command_polls_are_refused() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();
    for path in [
        "/bench/commands/",
        "/bench/commands/run-1",
        "/bench/commands/run-1/notanumber",
        "/bench/commands/BAD_RUN/0",
    ] {
        assert_eq!(get(&origin, path).status, 400, "{path}");
    }
    let posted = send(&origin, "POST", "/bench/commands/run-1/0", &[], b"");
    assert_eq!(posted.status, 405);
}

#[test]
fn wait_log_line_finds_a_line_without_consuming_the_log() {
    let fixture = start_fixture();
    let run = RunId::new("log-wait-1").unwrap();
    let origin = fixture.site.main_origin();
    let payload = format!(r#"{{"source":"page","run":"{run}","line":"mse-fmp4: handoff-ready"}}"#);
    let sender_origin = origin.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        send(
            &sender_origin,
            "POST",
            "/bench/log",
            &[],
            payload.as_bytes(),
        )
        .status
    });
    let line = fixture
        .site
        .wait_log_line(&run, "handoff-ready", Duration::from_secs(5))
        .expect("the line arrives");
    assert_eq!(line, "mse-fmp4: handoff-ready");
    assert_eq!(sender.join().unwrap(), 204);
    // Ожидание не расходует журнал: строка остаётся для take_logs.
    assert_eq!(fixture.site.take_logs(&run).len(), 1);

    let error = fixture
        .site
        .wait_log_line(&run, "never-logged", Duration::from_millis(200))
        .expect_err("no such line");
    assert!(
        matches!(error, SiteError::LogLineTimeout { ref needle, .. } if needle == "never-logged")
    );
}

#[test]
fn report_metrics_survive_the_round_trip_and_default_to_empty() {
    let fixture = start_fixture();
    let with_metrics = RunId::new("metrics-run").unwrap();
    let without = RunId::new("no-metrics-run").unwrap();
    let origin = fixture.site.main_origin();
    let body = format!(
        r#"{{"run":"{with_metrics}","scenario":"mse-fmp4","status":"pass","metrics":{{"real_frames":3,"window_s":9.5}}}}"#
    );
    send(&origin, "POST", "/bench/report", &[], body.as_bytes());
    let plain = format!(r#"{{"run":"{without}","scenario":"mse-fmp4","status":"pass"}}"#);
    send(&origin, "POST", "/bench/report", &[], plain.as_bytes());
    let report = fixture
        .site
        .wait_report(&with_metrics, Duration::from_secs(2))
        .unwrap();
    assert_eq!(report.metrics["real_frames"], 3.0);
    assert_eq!(report.metrics["window_s"], 9.5);
    let plain_report = fixture
        .site
        .wait_report(&without, Duration::from_secs(2))
        .unwrap();
    assert!(plain_report.metrics.is_empty());
}

/// `/bench/recording` — 404, пока файл не выбран; после выбора отдаёт его байты как есть (и по Range).
#[test]
fn recording_is_served_only_after_it_is_selected_and_byte_for_byte() {
    let fixture = start_fixture();
    let origin = fixture.site.main_origin();
    assert_eq!(get(&origin, "/bench/recording").status, 404);

    let recording_path = fixture._dir.path().join("page.fprec");
    fs::write(&recording_path, media_bytes()).unwrap();
    fixture.site.serve_recording(recording_path);
    let whole = get(&origin, "/bench/recording");
    assert_eq!(whole.status, 200);
    assert_eq!(whole.body, media_bytes());
    assert_eq!(
        whole
            .headers
            .get("access-control-allow-origin")
            .map(String::as_str),
        Some("*")
    );
    let slice = send(
        &origin,
        "GET",
        "/bench/recording",
        &[("Range", "bytes=10-19")],
        &[],
    );
    assert_eq!(slice.status, 206);
    assert_eq!(slice.body, media_bytes()[10..20]);
    assert_eq!(
        send(&origin, "POST", "/bench/recording", &[], b"x").status,
        405
    );
}
