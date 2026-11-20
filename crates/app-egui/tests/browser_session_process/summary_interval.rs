//! Сессия 16b3c2: короткий шаг сводки журнала для стенда моста (решение 386).
//!
//! Стенд судит «кадр дошёл до рендера» по строкам `Browser playback summary`; настоящий
//! шаг — 10 с, и строка стенда ждала его целиком. Тестовая переменная процесса
//! `FASTIPLAYER_TEST_BROWSER_SUMMARY_INTERVAL_MS` укорачивает шаг. Что доказывают тесты
//! (настоящий процесс, безоконно):
//! - с переменной промежуточные сводки идут с её шагом, и кадры в них растут — это та
//!   самая линейка стенда;
//! - недопустимое значение не принимается молча: предупреждение и настоящий шаг.

use super::generation_switch_clicks::first_generation_head;
use super::session_ends::send_all;
use super::*;

/// Переменная процесса — тот же текст, что у плеера (`summary_interval_override.rs`).
const SUMMARY_INTERVAL_VARIABLE: &str = "FASTIPLAYER_TEST_BROWSER_SUMMARY_INTERVAL_MS";
/// Короткий шаг теста, мс.
const SHORT_INTERVAL_MS: &str = "500";
/// Сколько промежуточных сводок тест ждёт подряд.
const PERIODIC_SUMMARIES: usize = 3;
/// Предел на три сводки с шагом 0,5 с: с запасом на установку поколения, но втрое меньше
/// настоящего шага 10 с — с прежним шагом первая промежуточная пришла бы позже.
const SHORT_SUMMARIES_DEADLINE: Duration = Duration::from_secs(4);
/// Строка процесса о принятом коротком шаге.
const SHORTENED_NOTICE: &str = "Browser playback summary interval shortened";
/// Строка процесса об отвергнутом значении.
const REFUSED_NOTICE: &str = "Ignoring the test summary interval override";
/// Правило в тексте отказа: допустимые пределы, мс.
const ALLOWED_RANGE: &str = "from 250 to 10000";
/// Строка промежуточной сводки.
const PERIODIC_SUMMARY: &str = "Browser playback summary kind=periodic";

#[test]
fn a_short_summary_interval_reports_growing_frames_every_step() {
    let Some(process) = SessionProcessUnderTest::start_with_env(
        "summary-interval",
        VideoMode::Headless,
        &[(SUMMARY_INTERVAL_VARIABLE, SHORT_INTERVAL_MS)],
    ) else {
        return;
    };
    let bus = &process.bus;
    // Строку о коротком шаге процесс пишет при старте, раньше «сеанс открыт».
    bus.count_in_log(SHORTENED_NOTICE);
    let mut socket = process.open_attached_session();
    assert_eq!(
        bus.log_count(SHORTENED_NOTICE),
        1,
        "the process did not report the shortened interval"
    );

    // Ролик фикстуры: 2,4 с видео скачано — хватает на несколько шагов по 0,5 с.
    let started = Instant::now();
    send_all(&mut socket, first_generation_head());
    let frames: Vec<u64> = (0..PERIODIC_SUMMARIES)
        .map(|_| log_number(&bus.wait_for_log_line(PERIODIC_SUMMARY), "presented_frames"))
        .collect();
    let elapsed = started.elapsed();

    assert!(
        elapsed <= SHORT_SUMMARIES_DEADLINE,
        "{PERIODIC_SUMMARIES} periodic summaries took {elapsed:?}: the short interval did not apply"
    );
    assert!(
        frames.windows(2).all(|pair| pair[0] <= pair[1]),
        "presented frames went backwards between summaries: {frames:?}"
    );
    assert!(
        frames.last().is_some_and(|last| *last > frames[0]),
        "frames did not grow between short summaries: {frames:?}"
    );
}

#[test]
fn an_out_of_range_interval_is_refused_with_a_warning() {
    // 100 мс короче тика часов (250 мс): цикл всё равно не проснулся бы чаще.
    let Some(process) = SessionProcessUnderTest::start_with_env(
        "summary-interval-refused",
        VideoMode::Headless,
        &[(SUMMARY_INTERVAL_VARIABLE, "100")],
    ) else {
        return;
    };
    let bus = &process.bus;
    // Предупреждение процесс пишет при старте, раньше «сеанс открыт».
    bus.forbid_in_log(&[SHORTENED_NOTICE]);
    bus.count_in_log(REFUSED_NOTICE);
    bus.count_in_log(ALLOWED_RANGE);
    let mut socket = process.open_attached_session();
    assert_eq!(bus.log_count(REFUSED_NOTICE), 1, "the refusal was silent");
    // Значение прислал не человек: в журнал попадает правило, а не само число.
    assert_eq!(
        bus.log_count(ALLOWED_RANGE),
        1,
        "the warning does not name the allowed range"
    );

    // Шаг остался настоящим: за 3 с игры промежуточной сводки нет.
    send_all(&mut socket, first_generation_head());
    bus.expect_no_log_line_for(Duration::from_secs(3), &[PERIODIC_SUMMARY]);
}
