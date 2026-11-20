//! Тесты правила «первый кадр без пустого кадра»: каждый ломает ОДНО свидетельство здорового журнала и
//! требует, чтобы приговор назвал именно его.

use super::*;

/// Прогон правила; возвращает список провалов.
fn failures_of(lines: &[String], expected_switches: usize) -> Vec<String> {
    let mut failures = Vec::new();
    media_switch_failures(lines, expected_switches, &mut failures);
    failures
}

fn clean_two_switches() -> Vec<String> {
    vec![
        switch_line(1, "first_frame", "no_previous_frame", 0, Some(90)),
        switch_line(2, "first_frame", "held", 0, Some(150)),
        switch_line(3, "first_frame", "held", 0, Some(1999)),
    ]
}

#[test]
fn clean_switches_pass() {
    assert_eq!(failures_of(&clean_two_switches(), 2), Vec::<String>::new());
}

#[test]
fn a_switch_exactly_at_the_limit_passes() {
    let lines = vec![switch_line(
        2,
        "first_frame",
        "held",
        0,
        Some(MAX_FIRST_FRAME_AFTER_SWITCH_MS),
    )];
    assert_eq!(failures_of(&lines, 1), Vec::<String>::new());
}

#[test]
fn one_empty_frame_fails_and_names_the_switch() {
    let mut lines = clean_two_switches();
    lines[2] = switch_line(3, "first_frame", "expired", 1, Some(300));
    let failures = failures_of(&lines, 2);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("media switch 3: 1 empty frame"),
        "{failures:?}"
    );
}

#[test]
fn a_late_first_frame_fails_with_the_value_and_the_limit() {
    let mut lines = clean_two_switches();
    lines[1] = switch_line(2, "first_frame", "held", 0, Some(2001));
    let failures = failures_of(&lines, 2);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("media switch 2"), "{failures:?}");
    assert!(failures[0].contains("2001 ms"), "{failures:?}");
    assert!(failures[0].contains("2000 ms"), "{failures:?}");
}

#[test]
fn a_first_frame_end_without_a_time_fails() {
    let mut lines = clean_two_switches();
    lines[1] = switch_line(2, "first_frame", "held", 0, None);
    assert!(failures_of(&lines, 2)[0].contains("gives no first_frame_ms"));
}

#[test]
fn no_lines_at_all_with_an_expected_relocation_fails() {
    // Старый плеер без строки: проверка не должна пройти вхолостую.
    let failures = failures_of(&[], 2);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("has 0 finished media switch line"),
        "{failures:?}"
    );
    assert!(failures[0].contains("needs 2"), "{failures:?}");
}

#[test]
fn a_row_with_no_relocations_needs_no_lines() {
    assert_eq!(failures_of(&[], 0), Vec::<String>::new());
}

#[test]
fn the_first_media_of_the_window_is_not_a_switch() {
    // Только первая строка окна: ни одной смены, хотя переезд ожидался.
    let lines = vec![switch_line(
        1,
        "first_frame",
        "no_previous_frame",
        0,
        Some(90),
    )];
    let failures = failures_of(&lines, 1);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("has 0 finished media switch line"),
        "{failures:?}"
    );
}

#[test]
fn the_first_media_of_the_window_is_not_judged_for_empty_frames() {
    // Первый ролик окна может иметь «пустые» кадры (прежнего кадра нет) — это не смена и не провал.
    let mut lines = clean_two_switches();
    lines[0] = switch_line(1, "first_frame", "no_previous_frame", 5, Some(9000));
    assert_eq!(failures_of(&lines, 2), Vec::<String>::new());
}

#[test]
fn failed_switches_do_not_count_as_a_successful_one() {
    let lines = vec![
        switch_line(1, "first_frame", "no_previous_frame", 0, Some(90)),
        switch_line(2, "load_failed", "released", 0, None),
        switch_line(3, "player_stopped", "held", 0, None),
    ];
    let failures = failures_of(&lines, 1);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("switch 2 ended LoadFailed"),
        "{failures:?}"
    );
    assert!(
        failures[0].contains("switch 3 ended PlayerStopped"),
        "{failures:?}"
    );
}

#[test]
fn an_unknown_value_is_a_parse_failure_not_a_silent_skip() {
    let mut lines = clean_two_switches();
    lines[1] = switch_line(2, "first_frame", "teleported", 0, Some(100));
    let failures = failures_of(&lines, 2);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("cannot read") && failure.contains("teleported")),
        "{failures:?}"
    );
}
