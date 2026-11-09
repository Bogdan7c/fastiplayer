//! Сквозной тест 13c: кнопка качества в окне браузерного режима видит смену качества.
//!
//! Запись `quality-switch-new-media-source` меняет ролик 64×64 на 96×96 новым `MediaSource`
//! (полосы `video-64-6s.mp4` / `video-96-6s.mp4`). Окно (на частном виртуальном дисплее kwin)
//! играет её, и по журналу процесса видно, что подпись кнопки качества сменилась с «64p» на
//! «96p» вместе с картинкой, которую видит зритель, а видео доиграно до рендера.
//!
//! Без окна (`kwin_wayland` не найден) тест, как остальные оконные, пропускается с
//! сообщением: `SessionProcessUnderTest::start` возвращает `None`.

use super::*;

/// Запись в фикстурах протокола.
const RECORDING: &str = "quality-switch-new-media-source";
/// Длина ролика записи.
const CLIP_US: u64 = 6_000_000;
/// Кадр ролика (25 к/с).
const FRAME_US: u64 = 40_000;
/// Начало строки журнала подписи качества (формат читает стенд моста).
const QUALITY_LINE: &str = "Browser quality shown";

/// Главное: окно показывает подпись по картинке на экране — «64p», потом «96p», — и ролик
/// доигран до показа кадров.
#[test]
fn the_quality_label_follows_the_picture_the_window_shows() {
    let Some(process) = SessionProcessUnderTest::start("quality-button", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    let (steps, _expected_bytes) = recorded_stream(RECORDING);

    let _messages = generation_switch::play_recording(&mut socket, steps);
    // Строки подписи идут в порядке показа: сначала «64p» (первое поколение), потом «96p».
    let first = bus.wait_for_log_line(&format!("{QUALITY_LINE} label=64p"));
    assert!(
        first.contains("width=64 height=64"),
        "the 64p line carries the picture size: {first}"
    );
    let second = bus.wait_for_log_line(&format!("{QUALITY_LINE} label=96p"));
    assert!(
        second.contains("width=96 height=96"),
        "the 96p line carries the picture size: {second}"
    );
    // Вес потока: в миг первого кадра поколение ещё не приведено к странице, поэтому первая
    // строка подписи может нести `unknown`; сразу за ней идёт строка с числом бит/с.
    let weighed = if bitrate_field(&second) == "unknown" {
        bus.wait_for_log_line(&format!("{QUALITY_LINE} label=96p"))
    } else {
        second
    };
    assert!(
        bitrate_field(&weighed)
            .parse::<u64>()
            .is_ok_and(|bits| bits > 0),
        "the shown segments have a weight in bits per second: {weighed}"
    );

    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    let presented = log_number(&summary, "presented_frames");
    let clip_frames = CLIP_US / FRAME_US;
    assert!(
        (clip_frames - 40..=clip_frames).contains(&presented),
        "the clip did not reach the renderer: {summary}"
    );
    bus.wait_for_log_line("Browser session process finished");
}

/// Значение поля `bitrate_bps` строки журнала подписи.
fn bitrate_field(line: &str) -> &str {
    line.split("bitrate_bps=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default()
}
