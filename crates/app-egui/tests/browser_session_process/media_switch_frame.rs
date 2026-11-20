//! Сквозной тест 16b3c (решение 380): кадр окна на смене медиа — до настоящего рендера.
//!
//! Запись `quality-switch-new-media-source` трижды меняет `MediaSource` посреди просмотра
//! (как следующий ролик ленты: новое поколение со своими init и сегментами). Окно браузерного
//! режима рисует её на частном виртуальном дисплее kwin настоящим рендером. На каждую смену
//! окно пишет строку замера `Browser window media switch …` (контракт — `report.rs` модуля
//! `state::present_frame_cache::media_switch_frame_hold`): сколько кадров окна ушло на экран
//! без видео до первого кадра нового медиа и что стало с кадром прежнего.
//!
//! Проверяется результат на экране: с удержанием (по умолчанию) между роликами нет ни одного
//! кадра окна без видео — до первого кадра нового медиа на экране прежний кадр. С выключенным
//! удержанием (`media_switch_frame_hold_ms = 0`) окно ведёт себя как до 16b3c: кадр прежнего
//! медиа уходит сразу, строка это отражает (`outcome=disabled`).
//!
//! Запуск: `cargo test -p app-egui --test browser_session_process media_switch -- --nocapture`.

use super::generation_switch::play_recording;
use super::*;

/// Запись в фикстурах протокола: первое поколение и три смены `MediaSource`.
const RECORDING: &str = "quality-switch-new-media-source";
/// Сколько смен медиа видит окно: первое медиа окна и три новых поколения.
const SWITCHES: usize = 4;
/// Заголовок строки замера (стабильный текстовый контракт для судьи стенда).
const SWITCH_LINE: &str = "Browser window media switch switch=";

/// Поле `key=value` строки замера как текст.
fn switch_field<'line>(line: &'line str, field: &str) -> &'line str {
    let needle = format!(" {field}=");
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("no {field} in: {line}"))
        + needle.len();
    line[start..]
        .split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("empty {field} in: {line}"))
}

/// Играет запись в окне с этим `config.toml` и возвращает строки замера всех смен по порядку.
/// `None` — `kwin_wayland` нет, тест пропускается.
fn switch_lines_in_a_window(
    name: &str,
    config: &fastiplayer_config::AppConfig,
) -> Option<Vec<String>> {
    let config_toml = config.to_pretty_toml().expect("config text");
    let process = SessionProcessUnderTest::start_window_with_config(name, &config_toml)?;
    let mut socket = process.open_attached_session();
    let (steps, _expected_bytes) = recorded_stream(RECORDING);
    play_recording(&mut socket, steps);
    read_player_messages_until(&mut socket, "the clock state ended", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            }
        )
    });
    let lines: Vec<String> = (0..SWITCHES)
        .map(|_| process.bus.wait_for_log_line(SWITCH_LINE))
        .collect();
    for line in &lines {
        eprintln!("{line}");
    }
    send_tab_gone_and_expect_close(&mut socket);
    process
        .bus
        .wait_for_log_line("Browser session process finished");
    Some(lines)
}

/// Общее для всех смен: номера по порядку, каждая кончилась первым кадром нового медиа (а не
/// обрывом), поколение рендера растёт, первое медиа окна держать было нечего.
fn assert_every_switch_reached_a_new_frame(lines: &[String]) {
    let mut previous_generation = 0;
    for (index, line) in lines.iter().enumerate() {
        assert_eq!(log_number(line, "switch"), index as u64 + 1, "{line}");
        assert_eq!(switch_field(line, "end"), "first_frame", "{line}");
        let generation = log_number(line, "render_generation");
        assert!(generation > previous_generation, "{line}");
        previous_generation = generation;
        // Мс до первого кадра — число (не `none`): смена кончилась показом.
        log_number(line, "first_frame_ms");
    }
    assert_eq!(
        switch_field(&lines[0], "outcome"),
        "no_previous_frame",
        "{}",
        lines[0]
    );
}

/// Главное: с удержанием (по умолчанию) ни одного кадра окна без видео между роликами —
/// кадр прежнего ролика держится до первого кадра нового (рендер по умолчанию — wgpu).
#[test]
fn the_browser_window_keeps_the_previous_frame_until_the_new_media_shows_one() {
    let config = fastiplayer_config::AppConfig::default();
    let Some(lines) = switch_lines_in_a_window("frame-hold", &config) else {
        return;
    };
    assert_every_switch_reached_a_new_frame(&lines);
    for line in &lines[1..] {
        assert_eq!(switch_field(line, "outcome"), "held", "{line}");
        assert_eq!(switch_field(line, "release"), "none", "{line}");
        assert_eq!(log_number(line, "empty_frames"), 0, "{line}");
    }
}

/// То же на рендере OpenGL ES 2.0 (профиль стенда браузерного режима): удержанный кадр
/// рисуется и этим рендером, без отказа кадра.
#[test]
fn the_gles2_browser_window_keeps_the_previous_frame_until_the_new_media_shows_one() {
    let mut config = fastiplayer_config::AppConfig::default();
    config.render.profile = fastiplayer_config::RenderProfile::OpenGles;
    let Some(lines) = switch_lines_in_a_window("frame-hold-gles2", &config) else {
        return;
    };
    assert_every_switch_reached_a_new_frame(&lines);
    for line in &lines[1..] {
        assert_eq!(switch_field(line, "outcome"), "held", "{line}");
        assert_eq!(log_number(line, "empty_frames"), 0, "{line}");
    }
}

/// То же с программным декодером (кадр — в памяти декодера FFmpeg, рендер загружает его сам):
/// удержанный кадр прежнего декодера рисуется, пока новый декодер выдаёт первый кадр.
#[test]
fn the_software_decoded_browser_window_keeps_the_previous_frame_until_the_new_media_shows_one() {
    let mut config = fastiplayer_config::AppConfig::default();
    config.video.preferred_backend = fastiplayer_config::VideoBackendPreference::Software;
    let Some(lines) = switch_lines_in_a_window("frame-hold-software", &config) else {
        return;
    };
    assert_every_switch_reached_a_new_frame(&lines);
    for line in &lines[1..] {
        assert_eq!(switch_field(line, "outcome"), "held", "{line}");
        assert_eq!(log_number(line, "empty_frames"), 0, "{line}");
    }
}

/// Удержание выключено настройкой: окно ведёт себя как до 16b3c — кадр прежнего ролика уходит
/// сразу, а замер всё равно пишется (так судья стенда сравнивает «до» и «после»).
#[test]
fn with_the_hold_disabled_the_window_drops_the_previous_frame_and_still_reports() {
    let mut config = fastiplayer_config::AppConfig::default();
    config.browser_session.media_switch_frame_hold_ms = 0;
    let Some(lines) = switch_lines_in_a_window("frame-hold-off", &config) else {
        return;
    };
    assert_every_switch_reached_a_new_frame(&lines);
    for line in &lines[1..] {
        assert_eq!(switch_field(line, "outcome"), "disabled", "{line}");
        assert_eq!(log_number(line, "held_frames"), 0, "{line}");
    }
}
