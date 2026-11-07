//! Сквозные тесты 10a1, часть 2: мастер, AES-128, перемотка, live, безопасность,
//! честные отказы, версии протокола и «вкладка ушла» у HLS-поколения.

use browser_bridge_protocol::session::player::StreamRejection;

use super::hls_page::{
    FRAGMENT_FRAMES, FRAGMENT_SECONDS, FragmentedMovie, HELLO_1_1, HlsPage, PageResource,
    announce_hls_generation, bridge_address, final_summary, fmp4_vod_playlist, fragmented_movie,
    open_attached_session_speaking,
};
use super::viewer_notices::REJECTION_NOTICE_LINE;
use super::*;

/// Номерок init и первого фрагмента в VOD-плейлисте фрагментированного ролика.
const INIT_HANDLE: u32 = 1;
const FIRST_FRAGMENT_HANDLE: u32 = 10;

/// Ресурсы VOD-ролика: корень `root`, init и фрагменты (номерки из констант выше).
fn vod_resources(movie: &FragmentedMovie, root: u32) -> Vec<(u32, PageResource)> {
    let mut resources = vec![
        (
            root,
            PageResource::Body(fmp4_vod_playlist(
                INIT_HANDLE,
                FIRST_FRAGMENT_HANDLE,
                movie.fragments.len(),
            )),
        ),
        (INIT_HANDLE, PageResource::Body(movie.init.clone())),
    ];
    for (index, fragment) in movie.fragments.iter().enumerate() {
        resources.push((fragment_handle(index), PageResource::Body(fragment.clone())));
    }
    resources
}

/// Номерок фрагмента `index` ролика.
fn fragment_handle(index: usize) -> u32 {
    FIRST_FRAGMENT_HANDLE + index as u32
}

/// Позиции часов «играет», мкс.
fn playing_positions(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect()
}

/// Отказы поколения, которые получила страница.
fn rejections(messages: &[PlayerSessionMessage]) -> Vec<StreamRejection> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::StreamRejected { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect()
}

/// Мастер с двумя вариантами (оба — один ролик под разными номерками): плеер выбрал
/// вариант, запросил его плейлист и сегменты по номеркам и показал кадры.
#[test]
fn a_master_with_two_variants_plays_one_variant_to_the_end() {
    let Some(process) = SessionProcessUnderTest::start("hls-master", VideoMode::Headless) else {
        return;
    };
    let movie = fragmented_movie();
    let master = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n\
         #EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=64x64,CODECS=\"avc1.42c00a,mp4a.40.2\"\n{}\n\
         #EXT-X-STREAM-INF:BANDWIDTH=600000,RESOLUTION=64x64,CODECS=\"avc1.42c00a,mp4a.40.2\"\n{}\n",
        bridge_address(100),
        bridge_address(200),
    );
    let mut resources = vec![(0, PageResource::Body(master.into_bytes()))];
    for variant_root in [100, 200] {
        // Каждый вариант — свой плейлист, свой init и свои фрагменты.
        let playlist =
            fmp4_vod_playlist(variant_root + 1, variant_root + 10, movie.fragments.len());
        resources.push((variant_root, PageResource::Body(playlist)));
        resources.push((variant_root + 1, PageResource::Body(movie.init.clone())));
        for (index, fragment) in movie.fragments.iter().enumerate() {
            resources.push((
                variant_root + 10 + index as u32,
                PageResource::Body(fragment.clone()),
            ));
        }
    }
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new(resources);
    page.serve_until_ended(&mut socket);

    let chosen_variants: Vec<u32> = [100, 200]
        .into_iter()
        .filter(|variant| page.requests_for(variant + 10) >= 1)
        .collect();
    assert!(
        !chosen_variants.is_empty(),
        "no variant segment was asked: {:?}",
        page.requests
    );
    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    let total = FRAGMENT_FRAMES * movie.fragments.len() as u64;
    assert!(
        (total - 3..=total).contains(&summary.presented_frames),
        "{}",
        summary.line
    );
    assert!(summary.audio_played_ms > 0, "{}", summary.line);
}

/// Шифрует AES-128-CBC с PKCS#7, как HLS-упаковщик.
fn encrypt_aes128_cbc(plaintext: &[u8], key: [u8; 16], iv: [u8; 16]) -> Vec<u8> {
    use aes::Aes128;
    use cbc::Encryptor;
    use cbc::cipher::block_padding::Pkcs7;
    use cbc::cipher::{BlockModeEncrypt, KeyIvInit};
    let mut buffer = vec![0_u8; plaintext.len() + 16];
    buffer[..plaintext.len()].copy_from_slice(plaintext);
    let length = Encryptor::<Aes128>::new((&key).into(), (&iv).into())
        .encrypt_padded::<Pkcs7>(&mut buffer, plaintext.len())
        .expect("encrypt fixture segment")
        .len();
    buffer.truncate(length);
    buffer
}

/// AES-128: ключ — ещё один номерок, сегменты зашифрованы, init открыт (MAP до KEY).
/// Кадры показаны — значит ключ пришёл через мост целым и сегменты расшифрованы.
#[test]
fn an_aes128_vod_gets_its_key_by_handle_and_plays() {
    let Some(process) = SessionProcessUnderTest::start("hls-aes", VideoMode::Headless) else {
        return;
    };
    let movie = fragmented_movie();
    let key = *b"bridge-key-16byt";
    let iv = [0x5a_u8; 16];
    let iv_hex: String = iv.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"{}\"\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"{}\",IV=0x{iv_hex}\n",
        bridge_address(INIT_HANDLE),
        bridge_address(2),
    );
    let mut resources = vec![
        (INIT_HANDLE, PageResource::Body(movie.init.clone())),
        (2, PageResource::Body(key.to_vec())),
    ];
    for (index, fragment) in movie.fragments.iter().enumerate() {
        playlist.push_str(&format!(
            "#EXTINF:{FRAGMENT_SECONDS:.3},\n{}\n",
            bridge_address(fragment_handle(index))
        ));
        resources.push((
            fragment_handle(index),
            PageResource::Body(encrypt_aes128_cbc(fragment, key, iv)),
        ));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    resources.push((0, PageResource::Body(playlist.into_bytes())));
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new(resources);
    page.serve_until_ended(&mut socket);

    assert!(page.requests_for(2) >= 1, "the AES-128 key was never asked");
    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    let total = FRAGMENT_FRAMES * movie.fragments.len() as u64;
    assert!(
        (total - 3..=total).contains(&summary.presented_frames),
        "{}",
        summary.line
    );
}

/// Перемотка страницей к `target_us` (плеер идёт следом).
fn page_seek(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, target_us: u64) {
    let text = format!(
        r#"{{"type":"seek_started","target_us":{target_us},"cause":{{"kind":"page_seek"}}}}"#
    );
    socket.send(Message::text(text)).expect("seek is sent");
}

/// Сколько первых фрагментов движок читает ещё при открытии (проба содержимого читает
/// ограниченный объём байтов через несколько сегментов): удерживать раньше нельзя —
/// поколение не откроется вовсе.
const READ_WHILE_OPENING: usize = 7;

/// VOD: перемотка вперёд и назад докачивает нужный сегмент (его номерок запрошен ПОСЛЕ
/// перемотки) и снова даёт кадры у цели.
///
/// Как сделать «нужный сегмент» видимым: движок качает вперёд так быстро, как отвечает
/// страница, и держит скачанное в кэше VOD. Поэтому страница молчит на хвост ролика
/// (движок дальше не скачает), а фрагмент, на котором движок встал, на время перемотки
/// вперёд отвечает обрывом сети — так он не попадает в кэш, и перемотка назад к нему
/// обязана скачать его заново. (Перемотка ждёт конца чтения, которое уже в пути: ответ
/// страницы на удержанный запрос нужен, иначе перемотка ждала бы срока молчания.)
#[test]
fn a_vod_seek_forward_and_back_fetches_the_target_segment_and_plays_there() {
    let Some(process) = SessionProcessUnderTest::start("hls-seek", VideoMode::Headless) else {
        return;
    };
    let movie = fragmented_movie();
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new(vod_resources(&movie, 0));
    page.hold((READ_WHILE_OPENING..movie.fragments.len()).map(fragment_handle));
    // Играет начало, дальше хвоста страница не отдаёт.
    page.serve_until(
        &mut socket,
        "playback of the first fragments",
        |_, message| {
            playing_positions(std::slice::from_ref(message))
                .first()
                .is_some_and(|position| *position >= 1_000_000)
        },
    );

    // Вперёд: к 3,7 с — это последний фрагмент (3,6–4,0 с), его ещё никто не просил.
    let forward_index = movie.fragments.len() - 1;
    let stalled_index = READ_WHILE_OPENING;
    assert_eq!(
        page.requests_for(fragment_handle(forward_index)),
        0,
        "{:?}",
        page.requests
    );
    let before_forward = page.requests.len();
    page_seek(&mut socket, 3_700_000);
    page.replace(fragment_handle(stalled_index), PageResource::NetworkError);
    page.release_held(&mut socket);
    let after_forward = page.serve_until(
        &mut socket,
        "playback at the forward target",
        |_, message| {
            playing_positions(std::slice::from_ref(message))
                .first()
                .is_some_and(|position| *position >= 3_700_000)
        },
    );
    assert!(
        page.handles_requested_after(before_forward)
            .contains(&fragment_handle(forward_index)),
        "the segment of the forward target was not fetched after the seek: {:?}",
        page.requests
    );
    assert!(!playing_positions(&after_forward).is_empty());

    // Назад: к 3,0 с — фрагмент, на котором движок стоял (2,8–3,2 с): целиком его страница
    // не отдавала ни разу, значит и в кэше движка его нет.
    let before_back = page.requests.len();
    page.replace(
        fragment_handle(stalled_index),
        PageResource::Body(movie.fragments[stalled_index].clone()),
    );
    page_seek(&mut socket, 3_000_000);
    let after_back = page.serve_until(
        &mut socket,
        "playback at the backward target",
        |_, message| {
            playing_positions(std::slice::from_ref(message))
                .first()
                .is_some_and(|position| (3_000_000..3_600_000).contains(position))
        },
    );
    assert!(
        page.handles_requested_after(before_back)
            .contains(&fragment_handle(stalled_index)),
        "the segment of the backward target was not fetched after the seek: {:?}",
        page.requests
    );
    assert!(!playing_positions(&after_back).is_empty());
    page.serve_until_ended(&mut socket);
    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    // Начало (до ~1 с), кусок у цели вперёд и от цели назад до конца (3,0–4,0 с).
    assert!(summary.presented_frames >= 45, "{}", summary.line);
    assert_eq!(summary.position_regressions, 0, "{}", summary.line);
    assert!(summary.audio_played_ms > 0, "{}", summary.line);
}

/// Снимок live-плейлиста: окно из `window` фрагментов с номера `first`, без ENDLIST.
fn live_snapshot(first: usize, window: usize) -> Vec<u8> {
    let mut text = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first}\n\
         #EXT-X-MAP:URI=\"{}\"\n",
        bridge_address(INIT_HANDLE)
    );
    for index in first..first + window {
        text.push_str(&format!(
            "#EXTINF:{FRAGMENT_SECONDS:.3},\n{}\n",
            bridge_address(fragment_handle(index))
        ));
    }
    text.into_bytes()
}

/// Live: скользящее окно. Корень запрашивается снова и снова (`EXT-X-MEDIA-SEQUENCE`
/// растёт), плеер просит сегменты, которых не было в начальном окне, и показывает кадры
/// дальше начального окна.
#[test]
fn a_live_sliding_window_is_refreshed_through_the_bridge_and_plays_past_the_first_window() {
    let Some(process) = SessionProcessUnderTest::start("hls-live", VideoMode::Headless) else {
        return;
    };
    let movie = fragmented_movie();
    let window = 3;
    // Окно сдвигается на два фрагмента (0,8 с) на каждое обновление корня (~1 с) и
    // последним снимком доходит до конца ролика.
    let snapshots: Vec<Vec<u8>> = [0, 2, 4, 6, movie.fragments.len() - window]
        .into_iter()
        .map(|first| live_snapshot(first, window))
        .collect();
    let mut resources = vec![
        (0, PageResource::Snapshots(snapshots)),
        (INIT_HANDLE, PageResource::Body(movie.init.clone())),
    ];
    for (index, fragment) in movie.fragments.iter().enumerate() {
        resources.push((fragment_handle(index), PageResource::Body(fragment.clone())));
    }
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new(resources);
    let last_window_segment = fragment_handle(movie.fragments.len() - 1);
    page.serve_until(
        &mut socket,
        "a segment beyond the first window",
        |page, _| page.requests_for(last_window_segment) >= 1,
    );
    // Ещё немного: показать то, что пришло за начальным окном.
    let deadline = Instant::now() + Duration::from_secs(2);
    page.serve_until(&mut socket, "a little more live playback", |_, _| {
        Instant::now() >= deadline
    });

    assert!(
        page.requests_for(0) >= 3,
        "the live root was not refreshed: {:?}",
        page.requests
    );
    send_tab_gone_and_expect_close(&mut socket);
    process
        .bus
        .wait_for_log_line("Playback drain finished class=played_to_end");
    let summary = final_summary(&process.bus);
    let first_window_frames = FRAGMENT_FRAMES * window as u64;
    assert!(
        summary.presented_frames > first_window_frames,
        "no frames beyond the first live window: {}",
        summary.line
    );
}

/// Безопасность: адреса не того вида в плейлисте (чужой хост, относительный путь,
/// `http` вместо `https`) НЕ уходят странице ни одним `fetch_resource` — она видит только
/// номерок корня, а поколение получает честный отказ, процесс не висит.
#[test]
fn foreign_addresses_in_a_playlist_are_never_asked_from_the_page() {
    let Some(process) = SessionProcessUnderTest::start("hls-foreign", VideoMode::Headless) else {
        return;
    };
    let playlist = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n\
         #EXTINF:1.0,\nhttps://example.invalid/x.ts\n\
         #EXTINF:1.0,\nrelative-segment.ts\n\
         #EXTINF:1.0,\nhttp://resource.fastiplayer-bridge.invalid/1\n\
         #EXT-X-ENDLIST\n";
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new([(0, PageResource::Body(playlist.as_bytes().to_vec()))]);
    let messages = page.serve_until(&mut socket, "stream_rejected", |_, message| {
        matches!(message, PlayerSessionMessage::StreamRejected { .. })
    });

    assert!(
        page.requests.iter().all(|request| request.resource == 0),
        "the player asked the page for something other than the root: {:?}",
        page.requests
    );
    assert_eq!(rejections(&messages).len(), 1, "{messages:?}");
    // Процесс жив и выходит штатно.
    send_tab_gone_and_expect_close(&mut socket);
    process
        .bus
        .wait_for_log_line("Browser session process finished");
}

/// Фон отказал качать корень (`refused`) → `stream_rejected{file_unavailable}`, а
/// зритель окна видит причину (в окне), сеанс не висит на спиннере.
#[test]
fn a_refused_root_is_rejected_as_file_unavailable() {
    refused_root_is_rejected(VideoMode::Headless);
}

/// То же в окне: зритель видит сообщение об отказе.
#[test]
fn a_refused_root_is_rejected_as_file_unavailable_in_a_window() {
    refused_root_is_rejected(VideoMode::Window);
}

fn refused_root_is_rejected(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("hls-refused", mode) else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new([(0, PageResource::Refused)]);
    let messages = page.serve_until(&mut socket, "stream_rejected", |_, message| {
        matches!(message, PlayerSessionMessage::StreamRejected { .. })
    });

    assert_eq!(
        rejections(&messages),
        vec![StreamRejection::FileUnavailable]
    );
    // Отказ фона не повторяется: корень спрошен ровно один раз.
    assert_eq!(page.requests_for(0), 1, "{:?}", page.requests);
    if mode == VideoMode::Window {
        let notice = process.bus.wait_for_log_line(REJECTION_NOTICE_LINE);
        assert!(notice.contains("reason=file_unavailable"), "{notice}");
    }
    send_tab_gone_and_expect_close(&mut socket);
}

/// 403 на сегменте посреди просмотра: показано то, что до него, затем честная ошибка
/// (`stream_rejected{playback_failed}`), а не вечный спиннер — с дедлайном ожидания.
///
/// Запрещён последний фрагмент (3,6 с): первые фрагменты движок читает ещё при открытии
/// (проба содержимого), и 403 там — это отказ открытия, а не ошибка посреди просмотра.
#[test]
fn a_forbidden_segment_in_the_middle_fails_the_playback_honestly() {
    let Some(process) = SessionProcessUnderTest::start("hls-403", VideoMode::Headless) else {
        return;
    };
    let movie = fragmented_movie();
    let forbidden_index = 9;
    let mut resources = vod_resources(&movie, 0);
    resources.retain(|(handle, _)| *handle != fragment_handle(forbidden_index));
    resources.push((
        fragment_handle(forbidden_index),
        PageResource::HttpError(403),
    ));
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new(resources);
    let messages = page.serve_until(&mut socket, "stream_rejected", |_, message| {
        matches!(message, PlayerSessionMessage::StreamRejected { .. })
    });

    assert_eq!(rejections(&messages), vec![StreamRejection::PlaybackFailed]);
    assert!(
        playing_positions(&messages)
            .iter()
            .any(|position| *position >= 1_000_000),
        "the fragments before the forbidden one were not played: {messages:?}"
    );
    send_tab_gone_and_expect_close(&mut socket);
}

/// Протокол 1.0: `hls_source_added` не принимается — сеанс закрыт кодом 4003 (сторож
/// поколений), плеер не попросил ни одного ресурса.
#[test]
fn an_hls_generation_of_a_protocol_1_0_page_closes_the_session() {
    let Some(process) = SessionProcessUnderTest::start("hls-v1-0", VideoMode::Headless) else {
        return;
    };
    // Обычный `open_attached_session` говорит `hello` 1.0.
    let mut socket = process.open_attached_session();
    announce_hls_generation(&mut socket, 0);
    assert_eq!(
        read_close_code(&mut socket),
        CloseCode::from(SessionCloseReason::ProtocolViolation.close_code())
    );
    let resources = process
        .bus
        .wait_for_log_line("Browser HLS resource requests");
    assert_eq!(
        log_number(&resources, "resource_requests_sent"),
        0,
        "{resources}"
    );
}

/// Вкладка ушла посреди HLS VOD: показанное до обрыва доиграно, процесс выходит по
/// «доиграно» (`played_to_end`), а не по отказу плеера.
#[test]
fn a_tab_gone_in_the_middle_of_an_hls_vod_plays_what_was_downloaded() {
    let Some(process) = SessionProcessUnderTest::start("hls-tab-gone", VideoMode::Headless) else {
        return;
    };
    let movie = fragmented_movie();
    let downloaded = READ_WHILE_OPENING;
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new(vod_resources(&movie, 0));
    // Дальше страница не успела: вкладка уходит, пока хвост в пути.
    page.hold((downloaded..movie.fragments.len()).map(fragment_handle));
    page.serve_until(
        &mut socket,
        "a request beyond the downloaded part",
        |page, _| page.requests_for(fragment_handle(downloaded)) >= 1,
    );
    send_tab_gone_and_expect_close(&mut socket);

    process.bus.wait_for_log_line_without(
        "Playback drain finished class=played_to_end",
        &["class=player_fault"],
    );
    let summary = final_summary(&process.bus);
    let downloaded_frames = FRAGMENT_FRAMES * downloaded as u64;
    assert!(
        (downloaded_frames - 5..=downloaded_frames).contains(&summary.presented_frames),
        "expected the {downloaded_frames} downloaded frames: {}",
        summary.line
    );
    assert!(summary.line.contains("state=ended"), "{}", summary.line);
}
