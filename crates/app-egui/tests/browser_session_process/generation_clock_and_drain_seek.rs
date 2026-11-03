//! Сессия 09a2: часы странице, пока новое поколение не показано (Д-4), и клик зрителя за
//! скачанным во время доигрывания (Д-5). Настоящий процесс, настоящий плеер.
//!
//! - **Д-4.** Страница открыла новый `MediaSource` без своей перемотки (реклама): у нового
//!   поколения нет цели, его данные приходят через несколько секунд. Всё это время
//!   элемент страницы стоит в нуле нового источника, а плеер ещё играет прежнее поколение
//!   до его отсечки. Странице не должны уходить ни позиция прежнего поколения, ни его
//!   `ended` (тень приняла бы их, и реклама «кончилась» бы, не начавшись, — решение 84).
//!   После прихода данных новое поколение показано и играет.
//! - **Д-5.** Вкладка ушла, окно досматривает скачанное. Клик зрителя далеко за скачанное
//!   (MPRIS `Seek` окна — тот же путь, что кнопка и таймлайн) отклоняется: позиция окна
//!   остаётся у текущей, зрителю — короткое сообщение «Only the downloaded part can be
//!   played», окно досматривает до конца скачанного и закрывается обычным путём (не по
//!   застою).
//!
//! Оконный тест нужен `kwin_wayland` (иначе SKIPPED) и `busctl`.

use super::generation_switch_clicks::{
    PageSourceBuffers, VIDEO_FRAGMENT_US, recorded_message, split_fmp4,
};
use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

/// MPRIS окна браузерного режима (04c).
const MPRIS_BROWSER_NAME: &str = "org.mpris.MediaPlayer2.fastiplayer.browser";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// Сколько видеофрагментов поколения 1 скачала страница (по 1,2 с, ключевой кадр в начале).
const FIRST_GENERATION_VIDEO_FRAGMENTS: usize = 2;
/// Сколько звуковых фрагментов (~1 с) поколения 1 скачала страница: звук длиннее
/// картинки, конец скачанного — у конца видео.
const FIRST_GENERATION_AUDIO_FRAGMENTS: usize = 3;

/// Поколение 1 записанной смены качества: объявление и начало ролика без конца потока
/// (страница ещё качает). `video_fragments`/`audio_fragments` — сколько фрагментов лежит.
fn first_generation_downloaded(video_fragments: usize, audio_fragments: usize) -> Vec<Message> {
    let audio = split_fmp4("audio-6s.mp4");
    let video = split_fmp4("video-64-6s.mp4");
    let mut page = PageSourceBuffers::new(1);
    let mut messages: Vec<Message> = ["001.json", "002.json", "003.json", "004.json"]
        .into_iter()
        .map(recorded_message)
        .collect();
    messages.push(page.frame(0, &audio.init));
    messages.push(page.frame(1, &video.init));
    for fragment in &audio.fragments[..audio_fragments] {
        messages.push(page.frame(0, fragment));
    }
    for fragment in &video.fragments[..video_fragments] {
        messages.push(page.frame(1, fragment));
    }
    messages
}

// ---- Д-4: часы странице, пока новый источник без цели ждёт данных ------------------

/// Пока плеер играет прежнее поколение, тест ждёт столько; за это время прежнее (1,2–2,4 с
/// скачанного от позиции ~0,5 с) успевает дойти до своей отсечки и кончиться.
const NEW_SOURCE_DATA_DELAY: Duration = Duration::from_secs(4);
/// Часы, ушедшие ДО того, как процесс узнал о новом поколении (уже летели), — не дальше
/// этого: тест объявляет поколение сразу после часов за 0,4 с, а тик — 250 мс.
const GAP_CLOCK_LIMIT_US: u64 = 1_000_000;
/// Новое поколение играет — часы «играет» дальше этого.
const NEW_GENERATION_PLAYS_PAST_US: u64 = 1_000_000;

/// Поколение 2 (новый источник без перемотки страницы): объявление и init-сегменты.
fn new_source_announced(page: &mut PageSourceBuffers) -> Vec<Message> {
    let audio = split_fmp4("audio-6s.mp4");
    let video = split_fmp4("video-96-6s.mp4");
    let mut messages: Vec<Message> = ["013.json", "014.json", "015.json", "016.json"]
        .into_iter()
        .map(recorded_message)
        .collect();
    messages.push(page.frame(0, &audio.init));
    messages.push(page.frame(1, &video.init));
    messages
}

/// Все медиа-фрагменты поколения 2 с начала и конец потока.
fn new_source_data(page: &mut PageSourceBuffers) -> Vec<Message> {
    let audio = split_fmp4("audio-6s.mp4");
    let video = split_fmp4("video-96-6s.mp4");
    let mut messages: Vec<Message> = audio
        .fragments
        .iter()
        .map(|fragment| page.frame(0, fragment))
        .collect();
    messages.extend(
        video
            .fragments
            .iter()
            .map(|fragment| page.frame(1, fragment)),
    );
    messages.push(Message::text(r#"{"type":"end_of_stream","generation":2}"#));
    messages
}

/// Читает всё, что плеер шлёт странице, в течение `period`.
fn read_player_messages_for(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    period: Duration,
) -> Vec<PlayerSessionMessage> {
    let deadline = Instant::now() + period;
    let mut messages = Vec::new();
    while Instant::now() < deadline {
        match socket.read() {
            Ok(Message::Text(text)) => {
                messages.push(decode_message(text.as_bytes()).expect("player session message"))
            }
            Ok(_) => {}
            Err(error) => panic!("the page connection failed while waiting: {error}"),
        }
    }
    messages
}

/// Часы из сообщений: (позиция, состояние).
fn clock_ticks(messages: &[PlayerSessionMessage]) -> Vec<(u64, ClockState)> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock {
                position_us, state, ..
            } => Some((position_us.as_micros(), *state)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_new_source_without_a_target_sees_no_old_clock_until_its_data_arrives() {
    new_source_without_a_target_sees_no_old_clock(VideoMode::Headless);
}

#[test]
fn a_new_source_without_a_target_sees_no_old_clock_until_its_data_arrives_in_a_window() {
    new_source_without_a_target_sees_no_old_clock(VideoMode::Window);
}

fn new_source_without_a_target_sees_no_old_clock(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("new-source-clock", mode) else {
        return;
    };
    let mut socket = process.open_attached_session();
    send_all(
        &mut socket,
        first_generation_downloaded(
            FIRST_GENERATION_VIDEO_FRAGMENTS,
            FIRST_GENERATION_AUDIO_FRAGMENTS,
        ),
    );
    wait_until_playing_past(&mut socket, 400_000);

    // Новый источник без перемотки страницы; данных нет ещё несколько секунд.
    let mut second = PageSourceBuffers::new(2);
    send_all(&mut socket, new_source_announced(&mut second));
    // Ждём не меньше `NEW_SOURCE_DATA_DELAY`, не читая лишнего после него.
    let gap = read_player_messages_for(&mut socket, NEW_SOURCE_DATA_DELAY);
    let gap_clocks = clock_ticks(&gap);
    assert!(
        gap_clocks.len() >= 8,
        "the page clock keeps ticking while the new source waits: {gap_clocks:?}"
    );
    assert!(
        gap_clocks
            .iter()
            .all(|(position, state)| *position <= GAP_CLOCK_LIMIT_US && *state != ClockState::Ended),
        "the page saw the replaced generation's position or end: {gap_clocks:?}"
    );
    // К концу ожидания прежнее поколение давно за своей отсечкой — часы говорят «0 / жду».
    assert_eq!(
        gap_clocks.last(),
        Some(&(0, ClockState::Waiting)),
        "the new source waits at zero: {gap_clocks:?}"
    );

    // Данные нового источника пришли: поколение показано и играет.
    send_all(&mut socket, new_source_data(&mut second));
    let played_at = wait_until_playing_past(&mut socket, NEW_GENERATION_PLAYS_PAST_US);
    assert!(played_at >= NEW_GENERATION_PLAYS_PAST_US);
    process
        .bus
        .wait_for_log_line("Browser stream generation loaded generation=2");
}

// ---- Д-5: клик за скачанным во время доигрывания ------------------------------------

/// Поколение 1 для доигрывания: видео 0–4,8 с, звук ~0–5 с из 6 с ролика — есть что
/// досматривать несколько секунд и есть куда кликнуть «за скачанное».
const DRAIN_VIDEO_FRAGMENTS: usize = 4;
const DRAIN_AUDIO_FRAGMENTS: usize = 5;
/// Конец скачанного для доигрывания (конец видео).
const DRAIN_PLAYABLE_END_US: u64 = DRAIN_VIDEO_FRAGMENTS as u64 * VIDEO_FRAGMENT_US;
/// Куда кликает зритель: за концом скачанного, но внутри ролика.
const CLICK_BEYOND_DOWNLOADED_US: u64 = 5_700_000;
/// Сколько ждать после клика, прежде чем смотреть позицию окна.
const AFTER_CLICK: Duration = Duration::from_secs(1);
/// Допуск позиции: секунда игры после клика плюс тик публикации MPRIS и проход цикла.
const POSITION_SLACK_US: u64 = 600_000;

/// Вызов `busctl` на частной шине; `Ok(stdout)` или текст ошибки.
fn busctl(bus: &PrivateBusWithBrowserSession, args: &[&str]) -> Result<String, String> {
    let output = Command::new("busctl")
        .arg(format!("--address={}", bus.address))
        .arg("--no-pager")
        .args(args)
        .output()
        .expect("busctl must be installed for the MPRIS tests");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Ждёт, пока MPRIS окна станет перематываемым.
fn wait_until_seekable(bus: &PrivateBusWithBrowserSession) {
    let deadline = Instant::now() + WAIT;
    while !busctl(
        bus,
        &[
            "get-property",
            MPRIS_BROWSER_NAME,
            MPRIS_PATH,
            MPRIS_PLAYER,
            "CanSeek",
        ],
    )
    .is_ok_and(|text| text.trim() == "b true")
    {
        assert!(
            Instant::now() < deadline,
            "the browser window MPRIS never became seekable"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Позиция окна по MPRIS (`x <мкс>`).
fn window_position_us(bus: &PrivateBusWithBrowserSession) -> u64 {
    let text = busctl(
        bus,
        &[
            "get-property",
            MPRIS_BROWSER_NAME,
            MPRIS_PATH,
            MPRIS_PLAYER,
            "Position",
        ],
    )
    .expect("MPRIS Position");
    text.trim()
        .strip_prefix("x ")
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("unexpected Position: {text}"))
}

/// Клик зрителя: относительная перемотка MPRIS (тот же путь, что кнопка окна).
fn viewer_seek_by(bus: &PrivateBusWithBrowserSession, offset_us: i64) {
    busctl(
        bus,
        &[
            // `--`: отрицательное смещение — не ключ busctl.
            "--",
            "call",
            MPRIS_BROWSER_NAME,
            MPRIS_PATH,
            MPRIS_PLAYER,
            "Seek",
            "x",
            &offset_us.to_string(),
        ],
    )
    .unwrap_or_else(|error| panic!("MPRIS Seek failed: {error}"));
}

#[test]
fn a_click_beyond_the_download_while_draining_is_refused_and_told_to_the_viewer() {
    let Some(process) = SessionProcessUnderTest::start("drain-click-beyond", VideoMode::Window)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(
        &mut socket,
        first_generation_downloaded(DRAIN_VIDEO_FRAGMENTS, DRAIN_AUDIO_FRAGMENTS),
    );
    wait_until_playing_past(&mut socket, 200_000);
    wait_until_seekable(bus);

    send_tab_gone_and_expect_close(&mut socket);
    let tab_left_at = Instant::now();

    let before_click_us = window_position_us(bus);
    assert!(
        before_click_us < DRAIN_PLAYABLE_END_US,
        "the drain still has something to play: {before_click_us}"
    );
    let offset_us = i64::try_from(CLICK_BEYOND_DOWNLOADED_US - before_click_us).expect("offset");
    viewer_seek_by(bus, offset_us);

    std::thread::sleep(AFTER_CLICK);
    let after_click_us = window_position_us(bus);
    assert!(
        after_click_us >= before_click_us
            && after_click_us
                <= before_click_us + AFTER_CLICK.as_micros() as u64 + POSITION_SLACK_US,
        "the window position did not stay with the playback: {before_click_us} → {after_click_us}"
    );
    assert!(
        after_click_us < DRAIN_PLAYABLE_END_US,
        "the position jumped beyond what was downloaded: {after_click_us}"
    );

    // Окно сказало зрителю, почему клик не исполнен.
    bus.wait_for_log_line(
        "Browser window tells the viewer: only the downloaded part can be played",
    );

    // Досмотр идёт своим чередом до конца скачанного — не застой из-за клика.
    let finished = bus.wait_for_log_line("Playback drain finished");
    assert!(
        finished.contains("class=played_to_end"),
        "the drain ended otherwise than by playing to the end: {finished}"
    );
    let left_to_play = Duration::from_micros(DRAIN_PLAYABLE_END_US - before_click_us);
    let played_for = tab_left_at.elapsed();
    assert!(
        played_for + Duration::from_secs(1) >= left_to_play,
        "the window stopped after {played_for:?}, {left_to_play:?} were left to play"
    );
    bus.wait_for_log_line("Browser session process finished");
}
