//! Перемотка ждёт страницу, пока та жива (решение владельца 150, пакет F 04d).
//!
//! Стенд 04f: YouTube 4K отдал данные нового места через ~12 с, а плеер сдавался через
//! 10 с («Seeking timed out») и вставал на паузу на СТАРОМ месте навсегда. Здесь срок
//! перемотки в config-е укорочен до [`COMMIT_TIMEOUT_MS`], а страница приносит байты цели
//! через [`PAGE_DELAY`] — заметно дольше. Ожидается: спиннер (часы странице — «жду
//! данных» в цели), без ошибки перемотки, и как только байты пришли — кадры цели на
//! показе и игра до конца; позиция ни разу не возвращается на старое место.
//!
//! Обычный (не браузерный) источник по-прежнему сдаётся по сроку — это проверяют
//! `player-core` (`seek_completion::live_source_wait`) и `pushed-media-source`
//! (`producer_wait`): у них нет «живого поставщика», и путь локального файла этим
//! пакетом не меняется.
//!
//! Нужен `kwin_wayland` для окна (иначе вариант с окном SKIPPED).

use super::generation_switch_clicks::{
    CLIP_US, FRAME_US, PLAYBACK_REFUSALS, PageSourceBuffers, START_WINDOW_US, VIDEO_FRAGMENT_US,
    is_ended, is_playing_from, page_seek, recorded_message, split_fmp4,
};
use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

/// Срок перемотки плеера в этих тестах (по умолчанию 10 с).
const COMMIT_TIMEOUT_MS: u64 = 1_500;
/// Через сколько страница приносит байты цели: вдвое дольше срока перемотки.
const PAGE_DELAY: Duration = Duration::from_secs(3);
/// Цель перемотки страницы: начало 5-го фрагмента видео (4,8 с) — у скачанного
/// (видео 0–2,4 с) её нет.
const TARGET_US: u64 = 4 * VIDEO_FRAGMENT_US;
/// Первый фрагмент звука, который страница кладёт для цели (~3 с: YouTube кладёт звук с
/// более ранней коробки).
const FIRST_AUDIO_FRAGMENT_FOR_TARGET: usize = 3;
/// Первый фрагмент видео с целью.
const FIRST_VIDEO_FRAGMENT_FOR_TARGET: usize = 4;
/// Строки журнала, которых при ожидании живой страницы быть не должно.
const SEEK_TIMEOUT_LINES: [&str; 2] = ["kind=SeekTimeout", "Final seek commit stopped on timeout"];

#[test]
fn a_page_seek_waits_for_a_slow_page_and_plays_at_the_target() {
    seek_waits_for_a_slow_page(VideoMode::Headless);
}

/// То же в окне браузерного режима (плеер `AppState`, спиннер окна).
#[test]
fn a_page_seek_waits_for_a_slow_page_and_plays_at_the_target_in_a_window() {
    seek_waits_for_a_slow_page(VideoMode::Window);
}

/// Поднимает процесс с коротким сроком перемотки в `config.toml`.
fn start_with_short_seek_timeout(mode: VideoMode) -> Option<SessionProcessUnderTest> {
    let name = "seek-waits-for-page";
    let mut config = fastiplayer_config::AppConfig::default();
    config.player.seek.commit_timeout_ms = COMMIT_TIMEOUT_MS;
    let config_text = config.to_pretty_toml().expect("config text");
    let video = match mode {
        VideoMode::Headless => TestVideo::Headless,
        VideoMode::Window => TestVideo::Window(window::VirtualWaylandDisplay::start(name)?),
    };
    let bus_name = match mode {
        VideoMode::Headless => format!("{name}-headless"),
        VideoMode::Window => format!("{name}-window"),
    };
    let bus = PrivateBusWithBrowserSession::start_with(&bus_name, Some(&config_text), &video);
    Some(SessionProcessUnderTest {
        bus,
        _video: video,
        mode,
    })
}

/// Страница одного поколения: объявление записи, init и скачанное начало (видео 0–2,4 с,
/// звук 0–3 с); номера кусков продолжаются при следующих байтах.
struct OneGenerationPage {
    buffers: PageSourceBuffers,
    audio: Vec<Vec<u8>>,
    video: Vec<Vec<u8>>,
}

impl OneGenerationPage {
    fn start() -> (Self, Vec<Message>) {
        let audio = split_fmp4("audio-6s.mp4");
        let video = split_fmp4("video-64-6s.mp4");
        let mut buffers = PageSourceBuffers::new(1);
        let mut messages: Vec<Message> = ["001.json", "002.json", "003.json", "004.json"]
            .into_iter()
            .map(recorded_message)
            .collect();
        messages.push(buffers.frame(0, &audio.init));
        messages.push(buffers.frame(1, &video.init));
        for fragment in &audio.fragments[..3] {
            messages.push(buffers.frame(0, fragment));
        }
        for fragment in &video.fragments[..2] {
            messages.push(buffers.frame(1, fragment));
        }
        let page = Self {
            buffers,
            audio: audio.fragments,
            video: video.fragments,
        };
        (page, messages)
    }

    /// Байты от цели до конца ролика и честный конец потока.
    fn bytes_from_the_target(&mut self) -> Vec<Message> {
        let mut messages = Vec::new();
        for fragment in &self.audio[FIRST_AUDIO_FRAGMENT_FOR_TARGET..] {
            messages.push(self.buffers.frame(0, fragment));
        }
        for fragment in &self.video[FIRST_VIDEO_FRAGMENT_FOR_TARGET..] {
            messages.push(self.buffers.frame(1, fragment));
        }
        messages.push(Message::text(r#"{"type":"end_of_stream","generation":1}"#));
        messages
    }
}

/// Часы странице: (позиция, состояние) по порядку.
fn clocks(messages: &[PlayerSessionMessage]) -> Vec<(u64, ClockState)> {
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

fn seek_waits_for_a_slow_page(mode: VideoMode) {
    let Some(process) = start_with_short_seek_timeout(mode) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    let (mut page, head) = OneGenerationPage::start();
    send_all(&mut socket, head);
    wait_until_playing_past(&mut socket, 1_000_000);

    // Страница перематывается сама туда, где у плеера данных нет, и долго молчит.
    socket
        .send(page_seek(TARGET_US))
        .expect("page seek is sent");
    std::thread::sleep(PAGE_DELAY);
    send_all(&mut socket, page.bytes_from_the_target());
    let messages = read_player_messages_until(&mut socket, "the end of the clip", is_ended);

    // Позиция не возвращается на старое место: с первых часов в цели (ожидание) и до
    // конца — только цель и дальше.
    let clocks = clocks(&messages);
    let reached_target = clocks
        .iter()
        .position(|(position, _)| *position + FRAME_US >= TARGET_US)
        .expect("the clock reached the target");
    assert!(
        clocks[reached_target..]
            .iter()
            .all(|(position, _)| *position + FRAME_US >= TARGET_US),
        "the position went back to the old place after the seek: {clocks:?}"
    );
    // Всё ожидание (дольше срока перемотки) часы говорили «жду данных» в цели.
    let waiting_at_target = clocks
        .iter()
        .filter(|clock| **clock == (TARGET_US, ClockState::Waiting))
        .count();
    assert!(
        waiting_at_target >= 4,
        "the page clock did not wait at the target while the page was slow: {clocks:?}"
    );
    // Как только байты пришли — игра от цели до конца.
    let first_playing = messages
        .iter()
        .find_map(|message| match message {
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } if position_us.as_micros() + FRAME_US >= TARGET_US => Some(position_us.as_micros()),
            _ => None,
        })
        .expect("the player played at the target");
    assert!(
        first_playing < TARGET_US + START_WINDOW_US,
        "the player did not start at the target: {first_playing}"
    );
    assert!(
        messages
            .iter()
            .any(|message| is_playing_from(message, TARGET_US))
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the stream was rejected"
    );

    send_tab_gone_and_expect_close(&mut socket);
    let forbidden: Vec<&str> = SEEK_TIMEOUT_LINES
        .iter()
        .chain(PLAYBACK_REFUSALS.iter())
        .copied()
        .collect();
    let summary = bus.wait_for_log_line_without("Browser playback summary kind=final", &forbidden);
    // Кадры цели (4,8–6 с = 30 кадров) дошли до показа поверх кадров до перемотки.
    let frames_from_the_target = (CLIP_US - TARGET_US) / FRAME_US;
    assert!(
        log_number(&summary, "presented_frames") >= frames_from_the_target + 20,
        "{summary}"
    );
}
