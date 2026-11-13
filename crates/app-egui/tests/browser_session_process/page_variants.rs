//! Сквозные тесты 14a: варианты страницы и «следующая серия» в настоящем процессе с окном
//! (частный `kwin_wayland`), до кадров на показе.
//!
//! - **Смена озвучки (решение 280):** поколение 1 играет, страница прислала меню озвучек,
//!   зритель выбирает дубляж → странице `select_variant{audio, dub}` → страница открывает
//!   поколение 2 с нуля (новый `MediaSource` и сброс на ноль) → плеер просит `seek` на
//!   секунду зрителя T → страница кладёт байты от T → поколение 2 играет от T, кадры на
//!   показе, позиция окна (MPRIS `Position`) — T ± 1 с.
//! - **Качество:** выбор в меню качеств → `select_variant{quality}`.
//! - **Next:** без `page_navigation` MPRIS `Next` странице ничего не шлёт; после
//!   `page_navigation{available}` — `next_episode`.
//!
//! Выбор в меню делает тестовая переменная `FASTIPLAYER_TEST_BROWSER_SELECT_VARIANT`
//! (`browser_session::window::variant_test_trigger`): виджетов меню ещё нет (14a P2b), а у
//! MPRIS выбора варианта нет. Переменная зовёт то же намерение окна, что пункт меню.
//!
//! Нужны `kwin_wayland` (иначе SKIPPED) и `busctl`.

use super::generation_switch_clicks::{
    CLIP_US, FRAME_US, MPRIS_BROWSER_NAME, MPRIS_PATH, MPRIS_PLAYER, PLAYBACK_REFUSALS,
    PageSourceBuffers, VIDEO_FRAGMENT_US, busctl, is_ended, is_playing_from, page_seek,
    recorded_message, second_generation_announced, second_generation_bytes_from, split_fmp4,
    wait_until_controllable, window_position_us,
};
use super::hls_page::open_attached_session_speaking;
use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

/// `hello` экстеншена протокола 1.7 (`page_navigation`).
const HELLO_1_7: &str = r#"{"type":"hello","protocol":{"major":1,"minor":7},"software_version":"0.1.0","capabilities":[]}"#;

/// `hello` экстеншена протокола 1.8 (`variant_outcome`, 15c2).
const HELLO_1_8: &str = r#"{"type":"hello","protocol":{"major":1,"minor":8},"software_version":"0.1.0","capabilities":[]}"#;

/// Тестовая переменная выбора варианта (формат — в `variant_test_trigger.rs`).
const SELECT_VARIANT_VARIABLE: &str = "FASTIPLAYER_TEST_BROWSER_SELECT_VARIANT";

/// С какой секунды зрителя (мс) тестовая переменная выбирает дубляж.
const SELECT_AT_MS: u64 = 3_500;

/// Подписи меню: уникальные слова, чтобы проверить, что журнал их не пишет (инвариант 7).
const ORIGINAL_LABEL: &str = "Original Q7";
const DUB_LABEL: &str = "Dub Z9";

/// Меню озвучек страницы: «Original» выбрана, «Dub» доступна.
fn audio_menu() -> Message {
    Message::text(format!(
        r#"{{"type":"variant_menu_changed","menu":{{"kind":"audio","options":[{{"id":"orig","label":"{ORIGINAL_LABEL}","selection":"selected"}},{{"id":"dub","label":"{DUB_LABEL}","selection":"available"}}]}}}}"#
    ))
}

/// Поколение 1: объявление и видео 0–4,8 с, звук 0–5 с; конца потока нет.
fn first_generation_long_head() -> Vec<Message> {
    let audio = split_fmp4("audio-6s.mp4");
    let video = split_fmp4("video-64-6s.mp4");
    let mut page = PageSourceBuffers::new(1);
    let mut messages: Vec<Message> = ["001.json", "002.json", "003.json", "004.json"]
        .into_iter()
        .map(recorded_message)
        .collect();
    messages.push(page.frame(0, &audio.init));
    messages.push(page.frame(1, &video.init));
    for fragment in &audio.fragments[..5] {
        messages.push(page.frame(0, fragment));
    }
    for fragment in &video.fragments[..4] {
        messages.push(page.frame(1, fragment));
    }
    messages
}

/// Ответ страницы на `seek` плеера: она начала перемотку (`seek_started{player_seek}`).
pub(super) fn player_seek_started(seek_id: u32, target_us: u64) -> Message {
    Message::text(format!(
        r#"{{"type":"seek_started","target_us":{target_us},"cause":{{"kind":"player_seek","seek_id":{seek_id}}}}}"#
    ))
}

/// Что страница делает сама сразу после открытия нового поколения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageAfterNewGeneration {
    /// Как YouTube: сразу говорит «я на нуле» (`seek_started{page_seek}` на 0).
    ResetsToZero,
    /// Как Ashdi: молчит — новый `MediaSource` просто стоит на нуле.
    StaysSilent,
}

/// Главное (решение 280): после смены озвучки новое поколение страницы встаёт на секунду
/// зрителя, хотя страница начала его с нуля, и играет оттуда до показа кадров.
#[test]
fn after_a_dub_switch_the_new_generation_plays_from_the_viewer_second() {
    play_dub_switch("dub-switch", PageAfterNewGeneration::ResetsToZero);
}

/// Ashdi: новое поколение начинается с нуля, а страница сама не перематывается вовсе.
/// Секунду зрителя даёт только главная постановка возврата (`aim_pending_for_viewer`):
/// повтора по своевольной перемотке страницы здесь нет, он не может её подменить.
#[test]
fn after_a_dub_switch_a_silent_page_still_gets_one_seek_to_the_viewer_second() {
    play_dub_switch("dub-switch-silent", PageAfterNewGeneration::StaysSilent);
}

/// Сценарий смены озвучки (см. описание модуля); `page` — что страница делает сама.
fn play_dub_switch(name: &str, page: PageAfterNewGeneration) {
    let Some(process) = SessionProcessUnderTest::start_with_env(
        name,
        VideoMode::Window,
        &[(SELECT_VARIANT_VARIABLE, &format!("audio:1@{SELECT_AT_MS}"))],
    ) else {
        return;
    };
    let bus = &process.bus;
    // Подписи — текст страницы: в журнал процесса не попадают.
    bus.forbid_in_log(&["Q7", "Z9"]);
    let mut socket = open_attached_session_speaking(&process, HELLO_1_7);
    send_all(&mut socket, vec![audio_menu()]);
    send_all(&mut socket, first_generation_long_head());
    wait_until_playing_past(&mut socket, 1_000_000);
    wait_until_controllable(bus);

    // Тестовая переменная выбирает «Dub» на ~3,5 с: странице уходит `select_variant`.
    let before_select = read_player_messages_until(&mut socket, "select_variant", |message| {
        matches!(message, PlayerSessionMessage::SelectVariant { .. })
    });
    let Some(PlayerSessionMessage::SelectVariant { kind, variant_id }) = before_select.last()
    else {
        panic!("the read ends with select_variant");
    };
    assert_eq!(
        (*kind, variant_id.as_str()),
        (browser_bridge_protocol::session::VariantKind::Audio, "dub")
    );
    bus.wait_for_log_line("Page variant selected kind=audio index=1");

    // Сайт качает новый поток: поколение 2 (новый MediaSource), может быть — сброс на ноль.
    let mut second = PageSourceBuffers::new(2);
    let mut announced = second_generation_announced(&mut second);
    if page == PageAfterNewGeneration::ResetsToZero {
        announced.push(page_seek(0));
    }
    send_all(&mut socket, announced);
    let until_seek = read_player_messages_until(&mut socket, "the carried page seek", |message| {
        matches!(message, PlayerSessionMessage::Seek { .. })
    });
    let Some(PlayerSessionMessage::Seek { seek_id, target_us }) = until_seek.last() else {
        panic!("the read ends with a seek");
    };
    let carried_us = target_us.as_micros();
    assert!(
        (SELECT_AT_MS * 1_000..4 * VIDEO_FRAGMENT_US).contains(&carried_us),
        "the seek goes to the second the viewer chose the dub at: {carried_us}"
    );
    bus.wait_for_log_line("Position carried to the new generation after a variant switch");

    // Страница подчиняется и кладёт байты поколения 2 от T до конца ролика.
    socket
        .send(player_seek_started(seek_id.0, carried_us))
        .expect("seek_started is sent");
    send_all(
        &mut socket,
        second_generation_bytes_from(&mut second, carried_us),
    );
    let until_playing = read_player_messages_until(
        &mut socket,
        "the new generation playing at the viewer second",
        |message| is_playing_from(message, carried_us),
    );
    // Позиция окна — секунда зрителя (± 1 с), а не ноль.
    let window_position = window_position_us(bus);
    assert!(
        window_position.abs_diff(carried_us) <= 1_000_000,
        "the window shows the viewer second {carried_us}, not {window_position}"
    );
    let Some(PlayerSessionMessage::Clock {
        position_us: first, ..
    }) = until_playing.last()
    else {
        panic!("a playing clock ends the read");
    };
    assert!(
        first.as_micros() < carried_us + 500_000,
        "the new generation starts at the viewer second: {first:?}"
    );
    let rest = read_player_messages_until(&mut socket, "the end of the clip", is_ended);
    let seeks_after_select = until_seek
        .iter()
        .chain(&until_playing)
        .chain(&rest)
        .filter(|message| matches!(message, PlayerSessionMessage::Seek { .. }))
        .count();
    assert_eq!(
        seeks_after_select, 1,
        "one page seek for the carried second"
    );
    assert!(
        !rest
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the new generation was rejected"
    );

    send_tab_gone_and_expect_close(&mut socket);
    let summary =
        bus.wait_for_log_line_without("Browser playback summary kind=final", &PLAYBACK_REFUSALS);
    assert_eq!(log_number(&summary, "loads"), 2, "{summary}");
    // Кадры поколения 2 от T до конца дошли до показа поверх кадров поколения 1.
    let second_generation_frames = (CLIP_US - carried_us) / FRAME_US;
    assert!(
        log_number(&summary, "presented_frames") >= second_generation_frames + 20,
        "{summary}"
    );
}

/// Выбор в меню качеств доходит до страницы как `select_variant{quality}`.
#[test]
fn a_quality_choice_reaches_the_page() {
    let Some(process) = SessionProcessUnderTest::start_with_env(
        "quality-variant",
        VideoMode::Window,
        &[(SELECT_VARIANT_VARIABLE, "quality:1@500")],
    ) else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_7);
    send_all(
        &mut socket,
        vec![Message::text(
            r#"{"type":"variant_menu_changed","menu":{"kind":"quality","options":[{"id":"q480","label":"480p","selection":"selected"},{"id":"q720","label":"720p","selection":"available"}]}}"#,
        )],
    );
    send_all(&mut socket, first_generation_long_head());
    let messages = read_player_messages_until(&mut socket, "select_variant", |message| {
        matches!(message, PlayerSessionMessage::SelectVariant { .. })
    });
    let Some(PlayerSessionMessage::SelectVariant { kind, variant_id }) = messages.last() else {
        panic!("the read ends with select_variant");
    };
    assert_eq!(
        (*kind, variant_id.as_str()),
        (
            browser_bridge_protocol::session::VariantKind::Quality,
            "q720"
        )
    );
    process
        .bus
        .wait_for_log_line("Page variant selected kind=quality index=1");
    send_tab_gone_and_expect_close(&mut socket);
}

/// MPRIS `Next`, как кнопка Next окна.
fn mpris_next(bus: &PrivateBusWithBrowserSession) {
    // Пока `CanGoNext` ложно, служба может отказать сама: это тоже «ничего не ушло».
    let _refused_or_done = busctl(
        bus,
        &["call", MPRIS_BROWSER_NAME, MPRIS_PATH, MPRIS_PLAYER, "Next"],
    );
}

/// Ждёт, пока MPRIS скажет `CanGoNext = true` (страница предложила серию).
fn wait_until_next_is_offered(bus: &PrivateBusWithBrowserSession) {
    let deadline = Instant::now() + WAIT;
    while !busctl(
        bus,
        &[
            "get-property",
            MPRIS_BROWSER_NAME,
            MPRIS_PATH,
            MPRIS_PLAYER,
            "CanGoNext",
        ],
    )
    .is_ok_and(|text| text.trim() == "b true")
    {
        assert!(
            Instant::now() < deadline,
            "MPRIS never offered Next after page_navigation{{available}}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Без `page_navigation` «Next» странице ничего не шлёт; после `available` — `next_episode`.
#[test]
fn next_asks_for_the_next_episode_only_after_the_page_offers_it() {
    let Some(process) = SessionProcessUnderTest::start("next-episode", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = open_attached_session_speaking(&process, HELLO_1_7);
    send_all(&mut socket, first_generation_long_head());
    wait_until_playing_past(&mut socket, 500_000);
    wait_until_controllable(bus);

    // Страница о следующей серии ничего не сказала: Next — пустое нажатие. Окно исполняет
    // команду шины на ближайшем проходе (миллисекунды); пауза — чтобы слово страницы ниже
    // точно пришло ПОСЛЕ этого нажатия.
    mpris_next(bus);
    std::thread::sleep(Duration::from_millis(700));
    socket
        .send(Message::text(
            r#"{"type":"page_navigation","next_episode":"available"}"#,
        ))
        .expect("page_navigation is sent");
    wait_until_next_is_offered(bus);
    mpris_next(bus);
    let messages = read_player_messages_until(&mut socket, "next_episode", |message| {
        matches!(message, PlayerSessionMessage::NextEpisode {})
    });
    // Сообщения странице идут по порядку: `next_episode` от первого нажатия пришёл бы раньше.
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(message, PlayerSessionMessage::NextEpisode {}))
            .count(),
        1,
        "only the press after page_navigation{{available}} asks for the next episode"
    );
    bus.wait_for_log_line("Next episode requested");
    send_tab_gone_and_expect_close(&mut socket);
}

/// 15c2: с протоколом 1.8 зритель сначала видит «идёт смена», а отказ страницы («у этой
/// озвучки нет такой серии») показывается причиной и снимает возврат на секунду зрителя.
/// Цепочка проверяется по журналу настоящего процесса: выбор → окно говорит `switching_audio`
/// → исход страницы → строка исхода (`matched=true`) → окно говорит про отказ → возврат снят.
#[test]
fn a_page_refusal_is_told_to_the_viewer_and_cancels_the_carry() {
    let Some(process) = SessionProcessUnderTest::start_with_env(
        "dub-refused",
        VideoMode::Window,
        &[(SELECT_VARIANT_VARIABLE, &format!("audio:1@{SELECT_AT_MS}"))],
    ) else {
        return;
    };
    let bus = &process.bus;
    bus.forbid_in_log(&["Q7", "Z9"]);
    let mut socket = open_attached_session_speaking(&process, HELLO_1_8);
    send_all(&mut socket, vec![audio_menu()]);
    send_all(&mut socket, first_generation_long_head());
    wait_until_playing_past(&mut socket, 1_000_000);
    wait_until_controllable(bus);

    let before_select = read_player_messages_until(&mut socket, "select_variant", |message| {
        matches!(message, PlayerSessionMessage::SelectVariant { .. })
    });
    let Some(PlayerSessionMessage::SelectVariant { kind, variant_id }) = before_select.last()
    else {
        panic!("the read ends with select_variant");
    };
    assert_eq!(
        (*kind, variant_id.as_str()),
        (browser_bridge_protocol::session::VariantKind::Audio, "dub")
    );
    bus.wait_for_log_line("Page variant selected kind=audio index=1");
    // 1.8: вместо мгновенного «Audio: …» окно говорит, что смена ещё идёт.
    bus.wait_for_log_line("Browser window tells the viewer about a page variant: switching_audio");

    // Страница отвечает, что у новой озвучки нет текущей серии.
    send_all(
        &mut socket,
        vec![Message::text(
            r#"{"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio","variant_id":"dub"},"outcome":"no_such_episode"}"#,
        )],
    );
    bus.wait_for_log_line("Page variant outcome kind=audio outcome=no_such_episode matched=true");
    // Журнал читается по порядку: возврат снимается раньше, чем окно получает тост.
    bus.wait_for_log_line("end=PageRefusedSelection");
    bus.wait_for_log_line(
        "Browser window tells the viewer about a page variant: refused_audio_no_such_episode",
    );
    send_tab_gone_and_expect_close(&mut socket);
}
