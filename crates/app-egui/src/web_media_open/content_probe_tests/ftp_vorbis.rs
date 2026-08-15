//! Вертикальная регрессия прямого FTP descriptor-а до production Vorbis PCM.

use std::{
    env, fs,
    io::{Read as _, Write as _},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    os::unix::fs::PermissionsExt as _,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use super::*;

/// Exact child path изолирует process-scoped fake yt-dlp document.
const FTP_VORBIS_CHILD_TEST_NAME: &str = "web_media_open::content_probe_tests::ftp_vorbis::ftp_ogg_with_ambient_http_headers_reaches_production_pcm";

/// Pinned direct-FTP format identity связывает hermetic и opt-in runtime assertions.
const FTP_VORBIS_FORMAT_ID: &str = "ogg";

/// Child получает exact FTP locator отдельно от redacted extractor document boundary.
const FTP_MEDIA_LOCATOR_ENV: &str = "RUSTIPLAYER_CONTENT_PROBE_FTP_LOCATOR";

/// Loopback FTP origin владеет media bytes и завершением worker-а.
struct FtpVorbisOrigin {
    /// Control endpoint нужен для descriptor URL.
    address: SocketAddr,
    /// Stop flag завершает неблокирующий accept loop.
    stop_requested: Arc<AtomicBool>,
    /// RETR counter доказывает настоящий проход через FTP provider.
    retrieval_count: Arc<AtomicUsize>,
    /// Join handle не оставляет detached test worker.
    worker: Option<JoinHandle<()>>,
}

impl FtpVorbisOrigin {
    /// Запускает минимальный passive FTP server на loopback ephemeral port.
    fn spawn(file_bytes: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind FTP Vorbis origin");
        listener
            .set_nonblocking(true)
            .expect("set FTP Vorbis listener nonblocking");
        let address = listener.local_addr().expect("read FTP Vorbis address");
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop_requested = Arc::clone(&stop_requested);
        let retrieval_count = Arc::new(AtomicUsize::new(0));
        let worker_retrieval_count = Arc::clone(&retrieval_count);
        let shared_file_bytes: Arc<[u8]> = Arc::from(file_bytes);
        let worker = thread::Builder::new()
            .name("content-probed-ftp-vorbis-origin".to_owned())
            .spawn(move || {
                while !worker_stop_requested.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((control_stream, _)) => {
                            let _ = serve_ftp_client(
                                control_stream,
                                shared_file_bytes.as_ref(),
                                worker_retrieval_count.as_ref(),
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => return,
                    }
                }
            })
            .expect("spawn FTP Vorbis origin worker");

        Self {
            address,
            stop_requested,
            retrieval_count,
            worker: Some(worker),
        }
    }

    /// Строит exact direct FTP locator для fake extractor format row.
    fn media_url(&self) -> String {
        format!("ftp://{}/content-probed.ogg", self.address)
    }

    /// Возвращает число начатых media transfer-ов.
    fn retrieval_count(&self) -> usize {
        self.retrieval_count.load(Ordering::SeqCst)
    }
}

impl Drop for FtpVorbisOrigin {
    /// Останавливает и присоединяет worker до уничтожения media bytes.
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join FTP Vorbis origin worker");
        }
    }
}

/// Pinned yt-dlp ambient HTTP headers не должны блокировать FTP/Ogg playback.
#[test]
fn ftp_ogg_with_ambient_http_headers_reaches_production_pcm() {
    if env::var_os(CHILD_PROCESS_MARKER_ENV).is_some() {
        let ftp_locator = env::var(FTP_MEDIA_LOCATOR_ENV).expect("missing child FTP locator");
        vorbis::assert_child_vorbis_reaches_pcm_at_locator(&ftp_locator, FTP_VORBIS_FORMAT_ID);
        return;
    }

    let origin = FtpVorbisOrigin::spawn(vorbis::large_vorbis_fixture());
    let fake_tools = TempDir::new().expect("create FTP Vorbis fake-tools directory");
    install_fake_ftp_yt_dlp(fake_tools.path());
    let media_url = origin.media_url();
    let extractor_document = format!(
        r#"{{"id":"ftp-vorbis","title":"FTP Vorbis","formats":[{{"format_id":"{FTP_VORBIS_FORMAT_ID}","url":"{}","protocol":"ftp","ext":"ogg","vcodec":"none","acodec":null,"http_headers":{{"User-Agent":"Mozilla/5.0","Accept":"text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8","Accept-Language":"en-us,en;q=0.5","Sec-Fetch-Mode":"navigate"}}}}]}}"#,
        media_url
    );
    let child_output =
        run_ftp_content_probe_child(fake_tools.path(), &media_url, extractor_document);

    assert_child_succeeded("FTP Ogg/Vorbis playback", &child_output);
    assert!(
        origin.retrieval_count() > 0,
        "production FTP provider обязан выполнить хотя бы один RETR"
    );
}

/// Запускает exact child с FTP locator и process-scoped extractor document-ом.
fn run_ftp_content_probe_child(
    fake_tools_directory: &Path,
    media_url: &str,
    extractor_document: String,
) -> std::process::Output {
    Command::new(env::current_exe().expect("current app-egui test binary"))
        .arg("--exact")
        .arg(FTP_VORBIS_CHILD_TEST_NAME)
        .arg("--nocapture")
        .env(CHILD_PROCESS_MARKER_ENV, "1")
        .env(FTP_MEDIA_LOCATOR_ENV, media_url)
        .env(YT_DLP_DOCUMENT_ENV, extractor_document)
        .env("PATH", path_with_fake_tools_first(fake_tools_directory))
        .output()
        .expect("spawn isolated FTP ContentProbed test child")
}

/// Fake extractor принимает только native FTP argv без HTTP impersonation policy.
fn install_fake_ftp_yt_dlp(fake_tools_directory: &Path) {
    let executable_path = fake_tools_directory.join("yt-dlp");
    let script = concat!(
        "#!/bin/sh\n",
        "set -eu\n",
        "test \"$#\" -eq 6\n",
        "test \"$1\" = \"--quiet\"\n",
        "test \"$2\" = \"--no-warnings\"\n",
        "test \"$3\" = \"--simulate\"\n",
        "test \"$4\" = \"--dump-single-json\"\n",
        "test \"$5\" = \"--no-playlist\"\n",
        "printf '%s\\n' \"${RUSTIPLAYER_CONTENT_PROBE_YTDLP_JSON:?missing fixture JSON}\"\n",
    );
    fs::write(&executable_path, script).expect("write strict FTP fake yt-dlp executable");
    let mut permissions = fs::metadata(&executable_path)
        .expect("read strict FTP fake yt-dlp metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable_path, permissions)
        .expect("mark strict FTP fake yt-dlp executable");
}

/// Обслуживает command lifecycle, нужный production FTP source для probe/read/seek.
fn serve_ftp_client(
    mut control_stream: TcpStream,
    file_bytes: &[u8],
    retrieval_count: &AtomicUsize,
) -> std::io::Result<()> {
    control_stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    control_stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    send_ftp_reply(&mut control_stream, "220 rustiplayer FTP fixture ready")?;
    let mut restart_offset = 0_usize;
    let mut passive_listener: Option<TcpListener> = None;

    loop {
        let command = read_ftp_command(&mut control_stream)?;
        let uppercase_command = command.to_ascii_uppercase();
        if uppercase_command.starts_with("USER ") {
            send_ftp_reply(&mut control_stream, "331 password required")?;
        } else if uppercase_command.starts_with("PASS ") {
            send_ftp_reply(&mut control_stream, "230 logged in")?;
        } else if uppercase_command == "SYST" {
            send_ftp_reply(&mut control_stream, "215 UNIX Type: L8")?;
        } else if uppercase_command == "TYPE I" || uppercase_command == "TYPE BIN" {
            send_ftp_reply(&mut control_stream, "200 binary type selected")?;
        } else if uppercase_command.starts_with("SIZE ") {
            send_ftp_reply(&mut control_stream, &format!("213 {}", file_bytes.len()))?;
        } else if uppercase_command.starts_with("REST ") {
            restart_offset = command[5..].trim().parse().unwrap_or(0);
            send_ftp_reply(&mut control_stream, "350 restart position accepted")?;
        } else if uppercase_command == "PASV" {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            send_ftp_reply(
                &mut control_stream,
                &format!(
                    "227 Entering Passive Mode (127,0,0,1,{},{})",
                    port / 256,
                    port % 256
                ),
            )?;
            passive_listener = Some(listener);
        } else if uppercase_command.starts_with("RETR ") {
            let Some(listener) = passive_listener.take() else {
                send_ftp_reply(&mut control_stream, "425 use PASV first")?;
                continue;
            };
            send_ftp_reply(&mut control_stream, "150 opening binary data connection")?;
            let (mut data_stream, _) = listener.accept()?;
            retrieval_count.fetch_add(1, Ordering::SeqCst);
            let bounded_offset = restart_offset.min(file_bytes.len());
            data_stream.write_all(&file_bytes[bounded_offset..])?;
            data_stream.shutdown(Shutdown::Both)?;
            restart_offset = 0;
            send_ftp_reply(&mut control_stream, "226 transfer complete")?;
        } else if uppercase_command == "QUIT" {
            send_ftp_reply(&mut control_stream, "221 goodbye")?;
            return Ok(());
        } else if uppercase_command == "NOOP"
            || uppercase_command.starts_with("OPTS ")
            || uppercase_command.starts_with("CLNT ")
        {
            send_ftp_reply(&mut control_stream, "200 command accepted")?;
        } else {
            send_ftp_reply(&mut control_stream, "502 command not implemented")?;
        }
    }
}

/// Пишет одну CRLF-terminated FTP reply строку.
fn send_ftp_reply(control_stream: &mut TcpStream, reply: &str) -> std::io::Result<()> {
    write!(control_stream, "{reply}\r\n")?;
    control_stream.flush()
}

/// Читает одну bounded command строку без сохранения CRLF.
fn read_ftp_command(control_stream: &mut TcpStream) -> std::io::Result<String> {
    const MAXIMUM_COMMAND_BYTES: usize = 4 * 1024;
    let mut command = String::new();
    let mut byte = [0_u8; 1];
    while command.len() < MAXIMUM_COMMAND_BYTES {
        control_stream.read_exact(&mut byte)?;
        match byte[0] {
            b'\n' => return Ok(command),
            b'\r' => {}
            command_byte if command_byte.is_ascii() => command.push(command_byte as char),
            _ => return Err(std::io::Error::from(std::io::ErrorKind::InvalidData)),
        }
    }
    Err(std::io::Error::from(std::io::ErrorKind::InvalidData))
}
