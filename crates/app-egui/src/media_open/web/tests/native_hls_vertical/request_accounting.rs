//! Initial root handoff отделён от последующих live refresh по порядку HTTP событий.

use super::ControlledHlsServer;

/// Неизменяемый accounting завершённого initial root → media handoff.
pub(in crate::media_open::web::tests) struct InitialRootAccounting {
    pub(in crate::media_open::web::tests) requests: usize,
    pub(in crate::media_open::web::tests) body_bytes: usize,
}

impl ControlledHlsServer {
    /// До первого media-запроса все root fetch принадлежат initial handoff.
    /// DASH refresh worker стартует после initial component open: поздний refresh
    /// не должен менять уже доказанный prefix, а duplicate handoff остаётся видимым.
    pub(in crate::media_open::web::tests) fn initial_root_accounting(
        &self,
        root_path: &str,
        initial_media_path: &str,
    ) -> Option<InitialRootAccounting> {
        let requests = self.requests.lock().expect("fixture request log");
        let first_media = requests
            .iter()
            .position(|request| request.path == initial_media_path)?;
        let mut accounting = InitialRootAccounting {
            requests: 0,
            body_bytes: 0,
        };
        for request in &requests[..first_media] {
            if request.path == root_path {
                accounting.requests += 1;
                accounting.body_bytes += request.response_body_bytes;
            }
        }
        Some(accounting)
    }
}

#[test]
fn initial_root_accounting_keeps_duplicates_but_excludes_later_refresh() {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let server = ControlledHlsServer::start(HashMap::from([
        ("/manifest.mpd".to_owned(), vec![b"manifest".to_vec()]),
        ("/init.mp4".to_owned(), vec![b"media".to_vec()]),
    ]));
    let fetch = |path: &str| {
        let mut stream = TcpStream::connect(server.address).expect("connect fixture");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read bound");
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: fixture\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .expect("send request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("complete response");
        assert!(response.starts_with(b"HTTP/1.1 200"));
    };
    assert!(
        server
            .initial_root_accounting("/manifest.mpd", "/init.mp4")
            .is_none()
    );
    fetch("/manifest.mpd");
    fetch("/manifest.mpd"); // Настоящий duplicate initial fetch обязан остаться ошибкой oracle-а.
    assert!(
        server
            .initial_root_accounting("/manifest.mpd", "/init.mp4")
            .is_none()
    );
    fetch("/init.mp4");
    fetch("/manifest.mpd"); // Разрешённый refresh после передачи initial media.
    let initial = server
        .initial_root_accounting("/manifest.mpd", "/init.mp4")
        .expect("initial handoff observed");
    assert_eq!(initial.requests, 2);
    assert_eq!(initial.body_bytes, 16);
    assert_eq!(server.request_count("/manifest.mpd"), 3);
}
