//! Срок ожидания документа выбранной строки субтитров страницы (мост, сессия 16b5, решение 397)
//! в настоящем процессе с окном (частный `kwin_wayland`).
//!
//! Зритель выбирает строку меню CC страницы; страница отвечает «done», а текст качает сама.
//! Что доказывают тесты:
//! - текст так и не пришёл — окно само, без единого события ввода и стоя на ПАУЗЕ, через срок
//!   ожидания говорит об этом (строка журнала рядом с тостом). До 16b5 ожидание было бессрочным,
//!   и поломка перехвата субтитров в экстеншене выглядела как «выбрано, но субтитров нет»;
//! - текст пришёл вовремя — документ становится выбором зрителя, и по сроку окно молчит.
//!
//! Выбор делает тестовая переменная `FASTIPLAYER_TEST_BROWSER_SELECT_VARIANT` (она зовёт то же
//! намерение окна, что строка меню). Нужен `kwin_wayland` (иначе SKIPPED).

use super::generation_switch_clicks::wait_until_controllable;
use super::hls_page::open_attached_session_speaking;
use super::page_subtitles::offered_document;
use super::page_variants::first_generation_long_head;
use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

/// `hello` экстеншена протокола 1.8 (`variant_outcome`).
const HELLO_1_8: &str = r#"{"type":"hello","protocol":{"major":1,"minor":8},"software_version":"0.1.0","capabilities":[]}"#;

/// Тестовая переменная выбора варианта (формат — в `variant_test_trigger.rs`).
const SELECT_VARIANT_VARIABLE: &str = "FASTIPLAYER_TEST_BROWSER_SELECT_VARIANT";

/// Строка 0 меню субтитров выбирается на этой секунде зрителя (мс).
const SELECT_LINE: &str = "subtitles:0@3500";

/// Подпись строки меню. Та же подпись у документа `offered_document`: по ней документ узнаётся.
const LINE_LABEL: &str = "English";

/// Строки журнала плеера.
const OUTCOME_DONE_LINE: &str = "Page variant outcome kind=subtitles outcome=done matched=true";
const TEXT_NOT_ARRIVED_LINE: &str = "Page subtitle line document did not arrive";
const LINE_ADOPTED_LINE: &str = "Page subtitle line document became the viewer's choice";

/// Срок ожидания по умолчанию (`PageDocumentWaitPolicy`), мс.
const DEFAULT_WAIT_LIMIT_MS: u64 = 10_000;

/// Меню субтитров страницы из одной строки, страница её ещё не включала.
fn subtitles_menu() -> Message {
    Message::text(format!(
        r#"{{"type":"variant_menu_changed","menu":{{"kind":"subtitles","options":[{{"id":"en","label":"{LINE_LABEL}","selection":"available"}}]}}}}"#
    ))
}

/// Страница отвечает на выбор строки: язык включён (текст она скачает сама — или нет).
fn line_selected_on_the_page() -> Message {
    Message::text(
        r#"{"type":"variant_outcome","request":{"kind":"select_variant","menu":"subtitles","variant_id":"en"},"outcome":"done"}"#,
    )
}

/// Окно играет, зритель выбрал строку субтитров, страница ответила «done». Дальше — дело теста.
fn window_after_a_line_choice(
    name: &str,
) -> Option<(
    SessionProcessUnderTest,
    WebSocket<MaybeTlsStream<TcpStream>>,
)> {
    let process = SessionProcessUnderTest::start_with_env(
        name,
        VideoMode::Window,
        &[(SELECT_VARIANT_VARIABLE, SELECT_LINE)],
    )?;
    // Подпись страницы в журнал не попадает (инвариант 7).
    process.bus.forbid_in_log(&[LINE_LABEL]);
    let mut socket = open_attached_session_speaking(&process, HELLO_1_8);
    send_all(&mut socket, vec![subtitles_menu()]);
    send_all(&mut socket, first_generation_long_head());
    wait_until_playing_past(&mut socket, 1_000_000);
    wait_until_controllable(&process.bus);
    let before_select = read_player_messages_until(&mut socket, "select_variant", |message| {
        matches!(message, PlayerSessionMessage::SelectVariant { .. })
    });
    let Some(PlayerSessionMessage::SelectVariant { kind, variant_id }) = before_select.last()
    else {
        panic!("the read ends with select_variant");
    };
    assert_eq!(
        (*kind, variant_id.as_str()),
        (
            browser_bridge_protocol::session::VariantKind::Subtitles,
            "en"
        )
    );
    send_all(&mut socket, vec![line_selected_on_the_page()]);
    process.bus.wait_for_log_line(OUTCOME_DONE_LINE);
    Some((process, socket))
}

/// Текст не пришёл: окно на паузе само просыпается по сроку и говорит об этом — ровно через срок.
#[test]
fn a_chosen_line_whose_text_never_comes_is_reported_by_the_paused_window_itself() {
    let Some((process, mut socket)) = window_after_a_line_choice("subtitle-line-no-text") else {
        return;
    };
    let bus = &process.bus;
    // Страница встала на паузу (так до плеера доходит пауза зрителя): часы окна стоят, кадров и
    // событий ввода больше нет, цикл окна спит. Разбудить его может только сам срок ожидания.
    let pause = std::fs::read_to_string(
        Path::new(FIXTURES).join("session/valid/page-page-playback-pause.json"),
    )
    .expect("pause fixture");
    socket
        .send(Message::text(pause.trim_end()))
        .expect("pause is sent");
    read_player_messages_until(&mut socket, "a paused clock", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Paused,
                ..
            }
        )
    });
    let reported = bus.wait_for_log_line_without(TEXT_NOT_ARRIVED_LINE, &[LINE_ADOPTED_LINE]);
    let waited_ms = log_number(&reported, "waited_ms");
    assert!(
        (DEFAULT_WAIT_LIMIT_MS..DEFAULT_WAIT_LIMIT_MS + 2_000).contains(&waited_ms),
        "the window reports at the deadline, not earlier and not at some later wake-up: {reported}"
    );
    // Сказано один раз: ожидание закончено.
    bus.expect_no_log_line_for(Duration::from_secs(2), &[TEXT_NOT_ARRIVED_LINE]);
    send_tab_gone_and_expect_close(&mut socket);
}

/// Текст пришёл вовремя: документ — выбор зрителя, и по сроку окно ничего не говорит.
#[test]
fn a_chosen_line_whose_text_comes_in_time_is_adopted_and_never_reported_missing() {
    let Some((process, mut socket)) = window_after_a_line_choice("subtitle-line-text-in-time")
    else {
        return;
    };
    let bus = &process.bus;
    send_all(&mut socket, offered_document());
    bus.wait_for_log_line_without(LINE_ADOPTED_LINE, &[TEXT_NOT_ARRIVED_LINE]);
    bus.expect_no_log_line_for(
        Duration::from_millis(DEFAULT_WAIT_LIMIT_MS + 2_000),
        &[TEXT_NOT_ARRIVED_LINE],
    );
    send_tab_gone_and_expect_close(&mut socket);
}
