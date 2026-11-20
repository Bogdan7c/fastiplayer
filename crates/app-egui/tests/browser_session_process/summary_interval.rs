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
/// Сколько промежуточных сводок тест собирает подряд (~4 с при шаге 0,5 с).
const PERIODIC_SUMMARIES: usize = 8;
/// Самый длинный допустимый промежуток между соседними сводками по отметкам журнала: шаг
/// 0,5 с плюс тик часов 0,25 с и запас. С настоящим шагом было бы 10 с.
const LONGEST_SUMMARY_GAP: Duration = Duration::from_millis(1_500);
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

    // Ролик фикстуры: 2,4 с видео скачано — хватает на несколько шагов по 0,5 с. Шаг меряется
    // отметками самих строк журнала, а не часами теста: медленный старт поколения на CI не
    // сдвигает промежуток между сводками.
    send_all(&mut socket, first_generation_head());
    let lines: Vec<String> = (0..PERIODIC_SUMMARIES)
        .map(|_| bus.wait_for_log_line(PERIODIC_SUMMARY))
        .collect();
    let moments: Vec<Duration> = lines.iter().map(|line| log_moment(line)).collect();
    let frames: Vec<u64> = lines
        .iter()
        .map(|line| log_number(line, "presented_frames"))
        .collect();

    let longest_gap = moments
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .max()
        .unwrap_or_default();
    assert!(
        longest_gap <= LONGEST_SUMMARY_GAP,
        "periodic summaries came {longest_gap:?} apart: the short interval did not apply ({moments:?})"
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

/// Время суток из отметки в начале строки журнала (`2026-11-20T16:17:52.496257Z  INFO …`).
fn log_moment(line: &str) -> Duration {
    let stamp = line.split_whitespace().next().expect("a stamped log line");
    let clock = stamp
        .split_once('T')
        .and_then(|(_, time)| time.strip_suffix('Z'))
        .unwrap_or_else(|| panic!("no UTC time in {line:?}"));
    let mut parts = clock.split(':');
    let mut field = || -> f64 {
        parts
            .next()
            .and_then(|part| part.parse().ok())
            .unwrap_or_else(|| panic!("a malformed time in {line:?}"))
    };
    let (hours, minutes, seconds) = (field(), field(), field());
    Duration::from_secs_f64(hours * 3_600.0 + minutes * 60.0 + seconds)
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
