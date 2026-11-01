//! Клики зрителя во время смены поколения (мост, сессия 04d, путь C, решения 146–147).
//!
//! Живая находка: страница (YouTube) пересоздала поток (новый `MediaSource` при смене
//! качества), зритель кликнул по таймлайну, пока новое поколение открывалось. Процесс
//! отдавал поколение плееру сразу на клик, где данных ещё не было; страница возвращалась
//! на своё место, но эта перемотка ждала показа, а показа без данных не бывает:
//! `VideoPreflightTimeout` → `playback_failed`. Заменённое поколение кончалось «честным»
//! концом — позиция прыгала в длительность.
//!
//! Что доказывают тесты (настоящий процесс, окно под частным `kwin_wayland`, клики — MPRIS
//! `Seek` окна, тем же путём, что кнопка и таймлайн):
//! - клик туда, где у нового поколения данных нет, даёт странице ровно один `seek`;
//!   страница отвечает СВОЕЙ перемоткой в другое место и кладёт байты только там — новое
//!   поколение играет оттуда (кадры доходят до показа), `stream_rejected` нет;
//! - заменённое поколение кончается обрывом: позиция окна (MPRIS `Position`) встаёт на
//!   конце скачанного, а не в длительности;
//! - хаотичная серия кликов (и перемотка самой страницы посреди неё) — по одному `seek`
//!   на клик, поколение встаёт на последний клик, как только там есть байты.
//!
//! Нужны `kwin_wayland` (иначе SKIPPED) и `busctl`.

use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

pub(super) const MPRIS_BROWSER_NAME: &str = "org.mpris.MediaPlayer2.fastiplayer.browser";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// Записанная смена качества: её сообщения объявления поколений 1 и 2 (полосы: 0 —
/// звук, 1 — видео; длительность 6 с).
pub(super) const RECORDING: &str = "streams/quality-switch-new-media-source";
/// Длина ролика фикстуры.
pub(super) const CLIP_US: u64 = 6_000_000;
/// Кадр ролика (25 к/с).
pub(super) const FRAME_US: u64 = 40_000;
/// Фрагмент видео фикстуры (ключевой кадр раз в 1,2 с).
pub(super) const VIDEO_FRAGMENT_US: u64 = 1_200_000;
/// Фрагмент звука фикстуры (~1 с; точные начала чуть позже целых секунд).
const AUDIO_FRAGMENT_US: u64 = 1_000_000;
/// Конец скачанного у поколения 1: видео — два фрагмента (0–2,4 с), звук — три.
const FIRST_GENERATION_PLAYABLE_END_US: u64 = 2 * VIDEO_FRAGMENT_US;
/// Первые часы «играет» нового поколения обязаны прийти не дальше этого от его старта
/// (часы 4 раза в секунду плюс установка).
pub(super) const START_WINDOW_US: u64 = 500_000;

/// Ролик фикстуры, разрезанный как его кладёт плеер страницы: init и фрагменты
/// (`moof` + `mdat`).
pub(super) struct SplitMedia {
    pub(super) init: Vec<u8>,
    pub(super) fragments: Vec<Vec<u8>>,
}

/// Режет fMP4 по верхним коробкам: всё до первой `moof` — init, дальше по фрагменту
/// на каждую `moof` вместе со следующими за ней коробками.
pub(super) fn split_fmp4(name: &str) -> SplitMedia {
    let bytes = std::fs::read(Path::new(FIXTURES).join("media").join(name)).expect("media");
    let mut offset = 0;
    let mut init_end = None;
    let mut fragment_starts = Vec::new();
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("size")) as usize;
        if &bytes[offset + 4..offset + 8] == b"moof" {
            init_end.get_or_insert(offset);
            fragment_starts.push(offset);
        }
        offset += size;
    }
    let init_end = init_end.expect("a fragmented MP4 has fragments");
    let fragments = fragment_starts
        .iter()
        .enumerate()
        .map(|(index, &start)| {
            let end = fragment_starts
                .get(index + 1)
                .copied()
                .unwrap_or(bytes.len());
            bytes[start..end].to_vec()
        })
        .collect();
    SplitMedia {
        init: bytes[..init_end].to_vec(),
        fragments,
    }
}

/// Страница: номера кусков по полосам поколения (подряд с нуля, как требует сторож).
pub(super) struct PageSourceBuffers {
    generation: StreamGeneration,
    next_sequence: [u64; 2],
}

impl PageSourceBuffers {
    pub(super) fn new(generation: u32) -> Self {
        Self {
            generation: StreamGeneration::new(generation).expect("generation"),
            next_sequence: [0, 0],
        }
    }

    /// Рамка байтов полосы `source_buffer` (0 — звук, 1 — видео).
    pub(super) fn frame(&mut self, source_buffer: u16, payload: &[u8]) -> Message {
        let sequence = &mut self.next_sequence[usize::from(source_buffer)];
        let header = DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
            generation: self.generation,
            source_buffer: SourceBufferIndex::new(source_buffer).expect("source buffer"),
            sequence: AppendSequence(*sequence),
        });
        *sequence += 1;
        Message::binary(encode_data_frame(&header, payload).expect("data frame"))
    }
}

pub(super) fn recorded_message(file: &str) -> Message {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join(RECORDING).join(file))
        .expect("fixture message");
    Message::text(text.trim_end().to_owned())
}

/// Поколение 1: объявление и скачанное начало ролика (видео 0–2,4 с, звук 0–3 с), конца
/// потока нет — страница ещё качает.
fn first_generation_head() -> Vec<Message> {
    let audio = split_fmp4("audio-6s.mp4");
    let video = split_fmp4("video-64-6s.mp4");
    let mut page = PageSourceBuffers::new(1);
    let mut messages: Vec<Message> = ["001.json", "002.json", "003.json", "004.json"]
        .into_iter()
        .map(recorded_message)
        .collect();
    messages.push(page.frame(0, &audio.init));
    messages.push(page.frame(1, &video.init));
    for fragment in &audio.fragments[..3] {
        messages.push(page.frame(0, fragment));
    }
    for fragment in &video.fragments[..2] {
        messages.push(page.frame(1, fragment));
    }
    messages
}

/// Поколение 2 (другое качество): объявление и init-сегменты — медиа ещё нет нигде.
fn second_generation_announced(page: &mut PageSourceBuffers) -> Vec<Message> {
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

/// Первый фрагмент видео, содержащий `at_us` (ключевой кадр не позже позиции).
fn video_fragment_at(at_us: u64) -> usize {
    usize::try_from(at_us / VIDEO_FRAGMENT_US).expect("index")
}

/// Фрагмент звука на один раньше содержащего `at_us`: начала звуковых фрагментов чуть
/// позже целых секунд, а YouTube кладёт звук с ещё более ранней коробки.
fn audio_fragment_before(at_us: u64) -> usize {
    usize::try_from((at_us / AUDIO_FRAGMENT_US).saturating_sub(1)).expect("index")
}

/// Какие фрагменты поколения 2 кладёт страница (номера фрагментов — с нуля).
struct SecondGenerationDownload {
    audio: std::ops::Range<usize>,
    video: std::ops::Range<usize>,
    /// Страница докачала ролик: честный конец потока поколения 2.
    ends_the_stream: bool,
}

/// Байты поколения 2 по `download` (сначала звук, затем видео, как кладёт страница).
fn second_generation_bytes(
    page: &mut PageSourceBuffers,
    download: SecondGenerationDownload,
) -> Vec<Message> {
    let audio = split_fmp4("audio-6s.mp4");
    let video = split_fmp4("video-96-6s.mp4");
    let mut messages = Vec::new();
    for fragment in
        &audio.fragments[download.audio.start..download.audio.end.min(audio.fragments.len())]
    {
        messages.push(page.frame(0, fragment));
    }
    for fragment in
        &video.fragments[download.video.start..download.video.end.min(video.fragments.len())]
    {
        messages.push(page.frame(1, fragment));
    }
    if download.ends_the_stream {
        messages.push(Message::text(r#"{"type":"end_of_stream","generation":2}"#));
    }
    messages
}

/// Байты поколения 2 с `target_us` до конца ролика (и конец потока).
fn second_generation_bytes_from(page: &mut PageSourceBuffers, target_us: u64) -> Vec<Message> {
    second_generation_bytes(
        page,
        SecondGenerationDownload {
            audio: audio_fragment_before(target_us)..usize::MAX,
            video: video_fragment_at(target_us)..usize::MAX,
            ends_the_stream: true,
        },
    )
}

pub(super) fn page_seek(target_us: u64) -> Message {
    Message::text(format!(
        r#"{{"type":"seek_started","target_us":{target_us},"cause":{{"kind":"page_seek"}}}}"#
    ))
}

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

/// Ждёт, пока MPRIS окна станет управляемым (кнопки появляются вместе со страницей).
fn wait_until_controllable(bus: &PrivateBusWithBrowserSession) {
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

/// Ждёт, пока позиция окна перестанет меняться (прежнее поколение доиграло скачанное).
fn wait_until_window_position_settles(bus: &PrivateBusWithBrowserSession) -> u64 {
    let deadline = Instant::now() + WAIT;
    let mut previous = window_position_us(bus);
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let current = window_position_us(bus);
        if current == previous && current > 0 {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "the window position never settled"
        );
        previous = current;
    }
}

/// Цели `seek`, о которых процесс попросил страницу.
pub(super) fn page_seek_targets(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Seek { target_us, .. } => Some(target_us.as_micros()),
            _ => None,
        })
        .collect()
}

pub(super) fn is_playing_from(message: &PlayerSessionMessage, position_us: u64) -> bool {
    matches!(
        message,
        PlayerSessionMessage::Clock {
            state: ClockState::Playing,
            position_us: position,
            ..
        } if position.as_micros() >= position_us
    )
}

pub(super) fn is_ended(message: &PlayerSessionMessage) -> bool {
    matches!(
        message,
        PlayerSessionMessage::Clock {
            state: ClockState::Ended,
            ..
        }
    )
}

/// Строки журнала, означающие отказ установки или плеера (так кончалась живая находка).
pub(super) const PLAYBACK_REFUSALS: [&str; 4] = [
    "could not start installing",
    "refused to install",
    "media install failed",
    "VideoPreflightTimeout",
];

/// Новое поколение доиграло с `start_us` до конца: первые часы «играет» не раньше
/// старта и в окне старта, до них — ни конца, ни позиции в длительности (`ended` прежнего
/// поколения страница не видит); последний конец — у конца ролика.
pub(super) fn assert_new_generation_played_from(messages: &[PlayerSessionMessage], start_us: u64) {
    let first_playing = messages
        .iter()
        .position(|message| is_playing_from(message, start_us))
        .expect("the new generation played");
    let PlayerSessionMessage::Clock {
        position_us: first, ..
    } = &messages[first_playing]
    else {
        panic!("a playing clock was found");
    };
    assert!(
        first.as_micros() < start_us + START_WINDOW_US,
        "the new generation did not start at {start_us}: {first:?}"
    );
    assert!(
        !messages[..first_playing].iter().any(is_ended),
        "the page saw an end before the new generation played: {messages:?}"
    );
    let ended_at = messages.iter().rev().find_map(|message| match message {
        PlayerSessionMessage::Clock {
            position_us,
            state: ClockState::Ended,
            ..
        } => Some(position_us.as_micros()),
        _ => None,
    });
    assert!(
        ended_at.is_some_and(|position| position + FRAME_US >= CLIP_US),
        "the new generation did not play to the end: {ended_at:?}"
    );
}

/// Главное (сценарий живой находки): поколение 1 играет → страница открывает поколение 2
/// → зритель кликает туда, где данных нет → страница перематывается сама в другое место и
/// кладёт байты только там → поколение 2 встаёт там → когда оно показано, клик зрителя
/// повторяется странице один раз (решение 149) → страница подчиняется, и поколение 2
/// играет от клика до конца, кадры на показе.
#[test]
fn a_click_during_a_generation_switch_is_repeated_once_after_the_page_returns_by_itself() {
    // Срок ожидания данных у цели (= срок коммита перемотки) — длиннее ожидания теста:
    // поколение обязано встать у страницы по её перемотке, а не по запасному сроку.
    let mut config = fastiplayer_config::AppConfig::default();
    config.player.seek.commit_timeout_ms = 120_000;
    let config_text = config.to_pretty_toml().expect("config text");
    let Some(process) =
        SessionProcessUnderTest::start_window_with_config("switch-click", &config_text)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, first_generation_head());
    wait_until_playing_past(&mut socket, 1_000_000);
    wait_until_controllable(bus);

    let mut second = PageSourceBuffers::new(2);
    send_all(&mut socket, second_generation_announced(&mut second));
    let mut messages = read_player_messages_until(&mut socket, "a clock tick", |message| {
        matches!(message, PlayerSessionMessage::Clock { .. })
    });
    // Клик на 4,2 с вперёд (~5,4–5,7 с): у поколения 2 нет ни одной коробки медиа, а
    // страница положит байты только у своей позиции (3,6–4,8 с) — у клика данных нет.
    viewer_seek_by(bus, 4_200_000);
    messages.extend(read_player_messages_until(
        &mut socket,
        "the page seek for the click",
        |message| matches!(message, PlayerSessionMessage::Seek { .. }),
    ));

    // Заменённое поколение 1 доиграло скачанное и встало на его конце (обрыв), а не в
    // длительности ролика.
    let settled = wait_until_window_position_settles(bus);
    assert!(
        settled <= FIRST_GENERATION_PLAYABLE_END_US + START_WINDOW_US,
        "the replaced generation's end jumped past what was downloaded: {settled}"
    );

    // Страница, как YouTube, возвращается на своё место и кладёт байты только там: видео
    // 3,6–4,8 с, звук 2–5 с. До клика они не достают.
    let click_us = page_seek_targets(&messages)[0];
    let page_position_us = 3 * VIDEO_FRAGMENT_US;
    socket
        .send(page_seek(page_position_us))
        .expect("page seek is sent");
    send_all(
        &mut socket,
        second_generation_bytes(
            &mut second,
            SecondGenerationDownload {
                audio: audio_fragment_before(page_position_us)..5,
                video: video_fragment_at(page_position_us)..4,
                ends_the_stream: false,
            },
        ),
    );
    // Решение 149: поколение 2 встало у страницы, а когда оно показано, клик зрителя
    // повторяется странице один раз (и плеер идёт к клику).
    messages.extend(read_player_messages_until(
        &mut socket,
        "the repeat of the viewer click",
        |message| matches!(message, PlayerSessionMessage::Seek { .. }),
    ));
    assert_eq!(
        page_seek_targets(&messages),
        vec![click_us, click_us],
        "the click and exactly one repeat of it"
    );
    // Страница подчинилась повтору: кладёт байты у клика и докачивает ролик.
    send_all(
        &mut socket,
        second_generation_bytes(
            &mut second,
            SecondGenerationDownload {
                audio: 5..usize::MAX,
                video: 4..usize::MAX,
                ends_the_stream: true,
            },
        ),
    );
    let after_repeat = read_player_messages_until(&mut socket, "the end of the clip", is_ended);
    assert_new_generation_played_from(&after_repeat, click_us);
    messages.extend(after_repeat);

    assert_eq!(
        page_seek_targets(&messages).len(),
        2,
        "no third page seek: {:?}",
        page_seek_targets(&messages)
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the new generation was rejected"
    );
    bus.wait_for_log_line_without("Browser playback ended", &PLAYBACK_REFUSALS);

    send_tab_gone_and_expect_close(&mut socket);
    let summary =
        bus.wait_for_log_line_without("Browser playback summary kind=final", &PLAYBACK_REFUSALS);
    assert_eq!(log_number(&summary, "loads"), 2, "{summary}");
    // Кадры поколения 2 от клика до конца дошли до показа поверх кадров поколения 1
    // (больше секунды до клика).
    let second_generation_frames = (CLIP_US - click_us) / FRAME_US;
    assert!(
        log_number(&summary, "presented_frames") >= second_generation_frames + 20,
        "{summary}"
    );
}

/// Требование 147: хаос во время смены поколения — серия кликов зрителя и перемотка самой
/// страницы посреди неё. Каждый клик — ровно один `seek` странице; поколение встаёт на
/// ПОСЛЕДНИЙ клик, как только страница положила там байты, и играет до конца.
#[test]
fn a_chaotic_click_series_during_a_generation_switch_lands_on_the_last_click() {
    let Some(process) = SessionProcessUnderTest::start("switch-chaos", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, first_generation_head());
    wait_until_playing_past(&mut socket, 1_000_000);
    wait_until_controllable(bus);

    let mut second = PageSourceBuffers::new(2);
    send_all(&mut socket, second_generation_announced(&mut second));
    let mut messages = read_player_messages_until(&mut socket, "a clock tick", |message| {
        matches!(message, PlayerSessionMessage::Clock { .. })
    });
    let clicks_us: [i64; 5] = [3_500_000, -500_000, 2_000_000, 4_000_000, 2_200_000];
    for (index, offset_us) in clicks_us.iter().enumerate() {
        viewer_seek_by(bus, *offset_us);
        if index == 2 {
            // Страница посреди серии перематывается сама (возвращается на своё место).
            socket
                .send(page_seek(1_200_000))
                .expect("page seek is sent");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    messages.extend(read_player_messages_until(
        &mut socket,
        "the page seek of every click",
        {
            let mut seen = 0;
            move |message| {
                if matches!(message, PlayerSessionMessage::Seek { .. }) {
                    seen += 1;
                }
                seen == clicks_us.len()
            }
        },
    ));
    let targets = page_seek_targets(&messages);
    let last_click_us = *targets.last().expect("a page seek");

    send_all(
        &mut socket,
        second_generation_bytes_from(&mut second, last_click_us),
    );
    messages.extend(read_player_messages_until(
        &mut socket,
        "the end of the clip",
        is_ended,
    ));
    assert_new_generation_played_from(&messages, last_click_us);

    assert_eq!(
        page_seek_targets(&messages),
        targets,
        "no page seek after the series: one per click, never a second for the install"
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the new generation was rejected"
    );
    send_tab_gone_and_expect_close(&mut socket);
    let summary =
        bus.wait_for_log_line_without("Browser playback summary kind=final", &PLAYBACK_REFUSALS);
    assert_eq!(log_number(&summary, "loads"), 2, "{summary}");
}

/// Конец init-сегмента `muxed.webm` (EBML-заголовок, Segment Info и Tracks — всё до
/// первого `Cluster`, ID `1F 43 B6 75`).
fn webm_init_end(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .position(|window| window == [0x1F, 0x43, 0xB6, 0x75])
        .expect("a WebM file has a cluster")
}

/// Поколение 2 — WebM VP9 + Opus одним SourceBuffer (как PeerTube и Twitch), длительность
/// 10 с; только init — медиа нет нигде.
fn vp9_generation_announced(page: &mut PageSourceBuffers, webm: &[u8]) -> Vec<Message> {
    vec![
        Message::text(r#"{"type":"generation_started","generation":2}"#),
        Message::text(
            r#"{"type":"duration_changed","generation":2,"duration":{"kind":"finite","duration_us":10000000}}"#,
        ),
        Message::text(
            r#"{"type":"source_buffer_added","generation":2,"source_buffer":0,"mime_type":"video/webm; codecs=\"vp9,opus\""}"#,
        ),
        page.frame(0, &webm[..webm_init_end(webm)]),
    ]
}

/// Тот же сценарий, что главный, с VP9 и гонкой открытия, как на живом YouTube: новое
/// поколение, сразу «я на T», зритель кликает, пока оно открывается, байты приходят только у
/// T, страница потом возвращается на T. Плееру, чтобы поставить VP9, нужен первый пакет
/// видео (профиль и глубина цвета — в заголовке кадра), поэтому поколение, отданное на
/// клике без данных, не ставится вовсе: проверено временной поломкой ворот — отказ
/// `VideoStreamConfiguration` («no VP9 header until the end of the stream») →
/// `stream_rejected`; на живом 4K то же место кончалось `VideoPreflightTimeout`. С воротами
/// поколение встаёт на T. Возврат страницы на T после клика — повод повторить клик один
/// раз (решение 149); страница возвращается снова — плеер остаётся на T без третьего
/// `seek`.
#[test]
fn a_click_during_a_switch_to_vp9_waits_for_data_instead_of_failing_the_install() {
    let mut config = fastiplayer_config::AppConfig::default();
    config.player.seek.commit_timeout_ms = 120_000;
    let config_text = config.to_pretty_toml().expect("config text");
    let Some(process) =
        SessionProcessUnderTest::start_window_with_config("switch-click-vp9", &config_text)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, first_generation_head());
    wait_until_playing_past(&mut socket, 1_000_000);
    wait_until_controllable(bus);

    let webm = std::fs::read(Path::new(FIXTURES).join("media/muxed.webm")).expect("media");
    let mut second = PageSourceBuffers::new(2);
    // Как YouTube: новое поколение, сразу «я на T» (03e) и init; байтов медиа ещё нет.
    let page_position_us = 3_000_000;
    let mut announced = vp9_generation_announced(&mut second, &webm);
    announced.insert(3, page_seek(page_position_us));
    send_all(&mut socket, announced);
    let mut messages = read_player_messages_until(&mut socket, "a clock tick", |message| {
        matches!(message, PlayerSessionMessage::Clock { .. })
    });
    // Зритель кликает, пока поколение открывается: на 4,2 с вперёд, где медиа не будет.
    viewer_seek_by(bus, 4_200_000);
    messages.extend(read_player_messages_until(
        &mut socket,
        "the page seek for the click",
        |message| matches!(message, PlayerSessionMessage::Seek { .. }),
    ));

    // Байты страницы приходят только у T: поколение открывается, но у клика данных нет.
    send_all(
        &mut socket,
        vec![
            Message::text(format!(
                r#"{{"type":"timestamp_offset_changed","generation":2,"source_buffer":0,"offset_us":{page_position_us}}}"#
            )),
            second.frame(0, &webm[webm_init_end(&webm)..]),
        ],
    );
    std::thread::sleep(Duration::from_millis(500));
    // Страница возвращается на своё место (живая находка: после «loaded») и докачала всё.
    send_all(
        &mut socket,
        vec![
            page_seek(page_position_us),
            Message::text(r#"{"type":"end_of_stream","generation":2}"#),
        ],
    );
    // Поколение встало у страницы (данные есть только там), а после показа клик
    // повторяется странице один раз (решение 149).
    messages.extend(read_player_messages_until(
        &mut socket,
        "the repeat of the viewer click",
        |message| matches!(message, PlayerSessionMessage::Seek { .. }),
    ));
    let targets = page_seek_targets(&messages);
    assert_eq!(targets.len(), 2, "the click and one repeat: {targets:?}");
    assert_eq!(targets[0], targets[1], "the repeat is the same click");
    // Страница не подчинилась и со второго раза — снова вернулась к себе: плеер остаётся
    // там, где страница, третьего `seek` нет.
    socket
        .send(page_seek(page_position_us))
        .expect("page seek is sent");
    let after_second_return = read_player_messages_until(
        &mut socket,
        "the VP9 generation playing at the page position",
        |message| is_playing_from(message, page_position_us),
    );
    let Some(PlayerSessionMessage::Clock {
        position_us: first, ..
    }) = after_second_return.last()
    else {
        panic!("a playing clock ends the read");
    };
    assert!(
        first.as_micros() < page_position_us + START_WINDOW_US,
        "the player did not stay where the page is: {first:?}"
    );
    messages.extend(after_second_return);
    messages.extend(read_player_messages_until(
        &mut socket,
        "the end of the VP9 bytes",
        is_ended,
    ));

    assert_eq!(
        page_seek_targets(&messages).len(),
        2,
        "no third page seek after the page disobeyed twice"
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the VP9 generation was rejected"
    );
    send_tab_gone_and_expect_close(&mut socket);
    let summary =
        bus.wait_for_log_line_without("Browser playback summary kind=final", &PLAYBACK_REFUSALS);
    assert_eq!(log_number(&summary, "loads"), 2, "{summary}");
}
