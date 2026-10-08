//! Сессия 16: обрыв сети посреди progressive HTTP (Range) воспроизведения.
//!
//! Loopback origin отдаёт часть файла, затем рвёт текущий ответ посередине и на
//! время обрыва перестаёт отвечать, после чего снова работает с поддержкой Range.
//! Тесты идут через production `open_direct_media`: transport registry →
//! `HttpRangeSource` → prefetch → Symphonia → фоновый `ProgressiveDemuxer`, то есть
//! ровно тот demuxer, который получает player.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use audio::{
    AudioDecoderConfig, AudioDecoderFactory, AudioPacketTiming, EncodedAudioPacket,
    ProductionAudioDecoderFactory,
};
use fastiplayer_config::{NetworkConfig, PlayerDemuxConfig};
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use source_core::CancellationToken;

/// Частота PCM fixture-а: 8 кГц mono 16 bit = 16 000 байт в секунду.
const FIXTURE_SAMPLE_RATE: u32 = 8_000;
/// Размер PCM data: ~3 МиБ, чтобы prefetch сделал несколько Range-запросов.
const FIXTURE_PCM_BYTES: usize = 3 * 1024 * 1024;
/// Самый долгий допустимый вызов `next_event`: поток player-а не должен ждать сеть.
const MAXIMUM_PLAYER_THREAD_STALL: Duration = Duration::from_millis(250);

/// Генерирует WAV, где каждый sample уникален по позиции (без повторов в окне
/// 64 Ki samples), поэтому склейку с неправильного смещения видно по байтам.
fn generated_wav(pcm_bytes: usize) -> (Vec<u8>, Vec<u8>) {
    let sample_data = (0..pcm_bytes / 2)
        .flat_map(|index| (index as u16).to_le_bytes())
        .collect::<Vec<_>>();
    let data_len = u32::try_from(sample_data.len()).expect("fixture помещается в u32");
    let mut bytes = Vec::with_capacity(44 + sample_data.len());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&FIXTURE_SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(FIXTURE_SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend_from_slice(&sample_data);
    (bytes, sample_data)
}

/// Как выглядит обрыв для клиента.
#[derive(Clone, Copy)]
enum OutageKind {
    /// Listener закрыт: ядро отвечает RST на SYN — настоящий `connection refused`.
    RefuseConnections,
    /// Соединение принимается и сразу закрывается без ответа (можно посчитать попытки).
    AcceptAndClose,
}

/// Origin с одним обрывом, который тест «взводит» после open.
struct FlakyRangeOrigin {
    /// Адрес, который переживает закрытие/повторный bind listener-а.
    address: SocketAddr,
    /// Флаг остановки accept-loop-а.
    stop: Arc<AtomicBool>,
    /// Принятые соединения (во время `RefuseConnections` ядро их не принимает).
    accepted_connections: Arc<AtomicUsize>,
    /// Следующий запрос после взвода рвётся посреди body и начинает обрыв.
    outage_armed: Arc<AtomicBool>,
    /// Join handle accept-loop-а.
    join_handle: Option<JoinHandle<()>>,
}

/// Параметры поведения origin-а, общие для accept-loop-а.
#[derive(Clone, Copy)]
struct OutagePlan {
    kind: OutageKind,
    duration: Duration,
}

impl FlakyRangeOrigin {
    fn spawn(body: Vec<u8>, plan: OutagePlan) -> Self {
        let listener = bind_nonblocking("127.0.0.1:0".parse().expect("loopback addr"));
        let address = listener.local_addr().expect("origin address");
        let stop = Arc::new(AtomicBool::new(false));
        let accepted_connections = Arc::new(AtomicUsize::new(0));
        let outage_armed = Arc::new(AtomicBool::new(false));
        let origin_state = OriginShared {
            stop: Arc::clone(&stop),
            accepted_connections: Arc::clone(&accepted_connections),
            outage_armed: Arc::clone(&outage_armed),
        };
        let join_handle = thread::Builder::new()
            .name("s16-flaky-origin".to_owned())
            .spawn(move || run_origin(listener, address, &body, plan, &origin_state))
            .expect("spawn origin");
        Self {
            address,
            stop,
            accepted_connections,
            outage_armed,
            join_handle: Some(join_handle),
        }
    }

    fn arm_outage(&self) {
        self.outage_armed.store(true, Ordering::SeqCst);
    }

    fn accepted_connections(&self) -> usize {
        self.accepted_connections.load(Ordering::SeqCst)
    }

    fn url(&self) -> String {
        format!("http://{}/drop.wav", self.address)
    }
}

impl Drop for FlakyRangeOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

/// Флаги, которые accept-loop делит с тестом.
struct OriginShared {
    stop: Arc<AtomicBool>,
    accepted_connections: Arc<AtomicUsize>,
    outage_armed: Arc<AtomicBool>,
}

fn bind_nonblocking(address: SocketAddr) -> TcpListener {
    // std на Unix выставляет SO_REUSEADDR, поэтому повторный bind того же порта
    // после обрыва не упирается в TIME_WAIT.
    let listener = TcpListener::bind(address).expect("bind origin");
    listener.set_nonblocking(true).expect("nonblocking origin");
    listener
}

fn run_origin(
    listener: TcpListener,
    address: SocketAddr,
    body: &[u8],
    plan: OutagePlan,
    shared: &OriginShared,
) {
    let mut listener = Some(listener);
    let mut outage_until: Option<Instant> = None;
    let mut outage_consumed = false;
    while !shared.stop.load(Ordering::SeqCst) {
        let outage_active = outage_until.is_some_and(|deadline| Instant::now() < deadline);
        if outage_until.is_some() && !outage_active {
            outage_until = None;
            if listener.is_none() {
                listener = Some(bind_nonblocking(address));
            }
        }
        let Some(active_listener) = listener.as_ref() else {
            thread::sleep(Duration::from_millis(5));
            continue;
        };
        match active_listener.accept() {
            Ok((mut stream, _)) => {
                shared.accepted_connections.fetch_add(1, Ordering::SeqCst);
                if outage_active {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let Some(range) = read_range_request(&mut stream) else {
                    continue;
                };
                if !outage_consumed && shared.outage_armed.load(Ordering::SeqCst) {
                    outage_consumed = true;
                    write_truncated_range(&mut stream, body, range);
                    outage_until = Some(Instant::now() + plan.duration);
                    if matches!(plan.kind, OutageKind::RefuseConnections) {
                        listener = None;
                    }
                } else {
                    write_range(&mut stream, body, range);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return,
        }
    }
}

/// Читает HTTP request и возвращает inclusive Range (`bytes=a-b`).
fn read_range_request(stream: &mut TcpStream) -> Option<(u64, u64)> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let text = String::from_utf8_lossy(&request);
    let range_line = text
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))?;
    let spec = range_line.split_once("bytes=")?.1.trim();
    let (start, end) = spec.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

fn range_header(body: &[u8], (start, end): (u64, u64)) -> (usize, usize, String) {
    let total = body.len() as u64;
    let end = end.min(total - 1);
    let header = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\n\
         Content-Length: {}\r\nETag: \"s16-fixture\"\r\nConnection: close\r\n\r\n",
        end - start + 1
    );
    (start as usize, end as usize, header)
}

fn write_range(stream: &mut TcpStream, body: &[u8], range: (u64, u64)) {
    let (start, end, header) = range_header(body, range);
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body[start..=end]);
}

/// Отдаёт заголовки и половину body, затем рвёт соединение.
fn write_truncated_range(stream: &mut TcpStream, body: &[u8], range: (u64, u64)) {
    let (start, end, header) = range_header(body, range);
    let half = start + (end - start) / 2;
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body[start..half]);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

/// Маленькие prefetch-окна и таймауты: обрыв наступает быстро и детерминированно.
fn drop_test_network_config(reconnect_wait: Duration) -> NetworkConfig {
    NetworkConfig {
        memory_cache_mb: 1,
        read_ahead_mb: 1,
        prefetch_initial_chunk_kb: 16,
        prefetch_chunk_mb: 1,
        connect_timeout_ms: 500,
        read_timeout_ms: 500,
        reconnect_wait_ms: u64::try_from(reconnect_wait.as_millis()).expect("ms fits u64"),
    }
}

/// Открывает fixture через production direct path и взводит обрыв.
fn open_and_arm(origin: &FlakyRangeOrigin, reconnect_wait: Duration) -> Box<dyn Demuxer + Send> {
    let locator = super::classify_direct_media_url(&origin.url())
        .expect("loopback wav URL классифицируется как direct media");
    let opened = super::open_direct_media(
        &locator,
        &drop_test_network_config(reconnect_wait),
        &PlayerDemuxConfig::default(),
        CancellationToken::new(),
    )
    .expect("до обрыва файл открывается");
    let parts = opened.into_runtime_parts();
    assert!(
        parts.demux_seek_port.is_some(),
        "seekable direct ресурс обязан отдать player-у seek port"
    );
    origin.arm_outage();
    parts.demuxer
}

/// Декодирует payload всех audio packet-ов production-декодером, как это делает
/// аудио-потребитель player-а, и возвращает PCM в формате выхода (f32).
fn decode_with_production_decoder(demuxer: &dyn Demuxer, packets: &[Vec<u8>]) -> Vec<f32> {
    let audio_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("WAV fixture содержит audio track");
    let mut decoder = ProductionAudioDecoderFactory::default()
        .create_decoder(AudioDecoderConfig::from_track_metadata(
            audio_track.id.get(),
            audio_track.codec_id.clone(),
            audio_track.sample_rate,
            audio_track.channels,
        ))
        .expect("production PCM decoder");
    let mut pcm = Vec::new();
    for payload in packets {
        let packet =
            EncodedAudioPacket::new(audio_track.id.get(), AudioPacketTiming::unknown(), payload);
        pcm.extend(decoder.decode(&packet).expect("PCM packet декодируется"));
    }
    pcm
}

/// Итог чтения demuxer-а до конца или до первой ошибки.
struct DemuxDrain {
    /// Склеенные payload-ы всех audio packet-ов.
    payload: Vec<u8>,
    /// Payload-ы по отдельности — для декодирования, как у аудио-потребителя.
    packets: Vec<Vec<u8>>,
    /// Ошибка demuxer-а, если чтение оборвалось.
    error: Option<anyhow::Error>,
    /// Самый долгий одиночный вызов `next_event` (на нём висел бы поток player-а).
    longest_next_event: Duration,
    /// Сколько раз demuxer честно сказал «данных пока нет» вместо блокировки.
    temporarily_unavailable_events: usize,
}

/// Читает demuxer так же, как player: неблокирующий `next_event` + пауза по hint-у.
fn drain_demuxer(demuxer: &mut dyn Demuxer) -> DemuxDrain {
    let mut drain = DemuxDrain {
        payload: Vec::new(),
        packets: Vec::new(),
        error: None,
        longest_next_event: Duration::ZERO,
        temporarily_unavailable_events: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let started = Instant::now();
        let event = demuxer.next_event();
        drain.longest_next_event = drain.longest_next_event.max(started.elapsed());
        match event {
            Ok(DemuxReadEvent::Packet(packet)) => {
                drain.payload.extend_from_slice(&packet.data);
                drain.packets.push(packet.data.to_vec());
            }
            Ok(DemuxReadEvent::EndOfStream) => return drain,
            Ok(DemuxReadEvent::TemporarilyUnavailable(hint)) => {
                drain.temporarily_unavailable_events += 1;
                thread::sleep(hint.retry_after());
            }
            Ok(DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_)) => {}
            Err(error) => {
                drain.error = Some(error);
                return drain;
            }
        }
    }
    panic!("demuxer не дошёл ни до EOS, ни до ошибки за 60 с");
}

/// Обрыв на 3 с посреди файла: после восстановления сети demuxer продолжает с
/// того же байта и дочитывает файл до конца без дыр и повторов, поток player-а
/// всё это время не блокируется (получает «данных пока нет»), а production
/// аудио-декодер получает из пакетов после обрыва ровно те сэмплы, что в файле.
#[test]
fn range_playback_survives_three_second_network_outage() {
    let (body, sample_data) = generated_wav(FIXTURE_PCM_BYTES);
    let origin = FlakyRangeOrigin::spawn(
        body,
        OutagePlan {
            kind: OutageKind::RefuseConnections,
            duration: Duration::from_secs(3),
        },
    );
    let mut demuxer = open_and_arm(&origin, Duration::from_secs(30));

    let drain = drain_demuxer(demuxer.as_mut());

    assert!(
        drain.error.is_none(),
        "обрыв сети не должен убивать воспроизведение: {:?}",
        drain.error
    );
    assert_eq!(drain.payload.len(), sample_data.len());
    assert!(
        drain.payload == sample_data,
        "байты после обрыва склеены неверно"
    );
    assert!(
        drain.temporarily_unavailable_events > 0,
        "во время обрыва player должен видеть «данных пока нет»"
    );
    assert!(
        drain.longest_next_event < MAXIMUM_PLAYER_THREAD_STALL,
        "поток player-а заблокирован на {:?}",
        drain.longest_next_event
    );

    let pcm = decode_with_production_decoder(demuxer.as_ref(), &drain.packets);
    let expected_samples = sample_data.len() / 2;
    assert_eq!(
        pcm.len(),
        expected_samples,
        "аудио-потребитель получил все сэмплы"
    );
    for (index, sample) in pcm.iter().enumerate() {
        // Fixture: sample с номером n равен (n mod 65536) как i16 → f32 в [-1, 1).
        let expected = f32::from(index as u16 as i16) / 32_768.0;
        assert!(
            (sample - expected).abs() < 1.0e-6,
            "сэмпл {index}: {sample} вместо {expected}"
        );
    }
}

/// Обрыв дольше бюджета: скачанное доигрывается, затем ошибка, которую player
/// распознаёт как «пропала сеть» (`io::ErrorKind::NetworkDown` в цепочке).
#[test]
fn outage_longer_than_budget_fails_with_network_down_after_buffered_bytes() {
    let (body, sample_data) = generated_wav(FIXTURE_PCM_BYTES);
    let origin = FlakyRangeOrigin::spawn(
        body,
        OutagePlan {
            kind: OutageKind::RefuseConnections,
            duration: Duration::from_secs(30),
        },
    );
    let started = Instant::now();
    let mut demuxer = open_and_arm(&origin, Duration::from_millis(1_500));

    let drain = drain_demuxer(demuxer.as_mut());

    let error = drain.error.expect("сеть не вернулась за бюджет");
    let network_down = error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NetworkDown)
    });
    assert!(network_down, "ошибка не помечена как сетевая: {error:#}");
    assert!(
        started.elapsed() >= Duration::from_millis(1_500),
        "ошибка не раньше бюджета ожидания"
    );
    assert!(!drain.payload.is_empty(), "скачанное до обрыва доиграно");
    assert!(
        sample_data.starts_with(&drain.payload),
        "доигранные байты — точное начало файла"
    );
    assert!(drain.longest_next_event < MAXIMUM_PLAYER_THREAD_STALL);
}

/// Закрытие media во время ожидания: drop не блокирует вызывающий поток, а
/// повторы прекращаются — новых подключений к серверу больше нет.
#[test]
fn closing_media_during_outage_stops_reconnects_without_blocking() {
    let (body, _sample_data) = generated_wav(FIXTURE_PCM_BYTES);
    let origin = FlakyRangeOrigin::spawn(
        body,
        OutagePlan {
            kind: OutageKind::AcceptAndClose,
            duration: Duration::from_secs(60),
        },
    );
    let mut demuxer = open_and_arm(&origin, Duration::from_secs(60));
    // Читаем, пока демуксер не начнёт ждать сеть (буфер исчерпан, идут повторы).
    let waiting_deadline = Instant::now() + Duration::from_secs(20);
    let accepted_before_wait = origin.accepted_connections();
    loop {
        assert!(Instant::now() < waiting_deadline, "обрыв так и не наступил");
        match demuxer.next_event().expect("до бюджета ошибок нет") {
            DemuxReadEvent::TemporarilyUnavailable(_)
                if origin.accepted_connections() > accepted_before_wait + 2 =>
            {
                break;
            }
            DemuxReadEvent::EndOfStream => panic!("EOS раньше обрыва"),
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }

    let drop_started = Instant::now();
    drop(demuxer);
    let drop_duration = drop_started.elapsed();
    // Даём фоновым потокам заметить отмену; дальнейшие повторы должны прекратиться.
    thread::sleep(Duration::from_millis(500));
    let accepted_after_cancel = origin.accepted_connections();
    thread::sleep(Duration::from_secs(3));

    assert!(
        drop_duration < MAXIMUM_PLAYER_THREAD_STALL,
        "закрытие media заблокировано на {drop_duration:?}"
    );
    assert_eq!(
        origin.accepted_connections(),
        accepted_after_cancel,
        "после закрытия media повторы в сеть продолжаются"
    );
}
