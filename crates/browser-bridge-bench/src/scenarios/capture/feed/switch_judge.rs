//! Правило «первый кадр нового ролика без пустого кадра» (16b3c, решение 380) для всех строк ленты, у
//! которых окно меняет ролик.
//!
//! Плеер пишет строку `Browser window media switch …` на каждую смену медиа (разбор — в
//! `observe::media_switch_log`). Правило судит ТОЛЬКО смены, кончившиеся первым кадром нового ролика
//! (`end=first_frame`); первая строка окна (`outcome=no_previous_frame`) — первый ролик, а не смена.
//!
//! Для каждой такой смены:
//!  * `empty_frames == 0`: ни один кадр окна не ушёл на экран без видео, пока ждали новый ролик
//!    (зритель не видит «чёрной вспышки» между роликами);
//!  * `first_frame_ms` не больше [`MAX_FIRST_FRAME_AFTER_SWITCH_MS`]: новый ролик встал «сразу после
//!    переезда», а не после долгого ожидания, которое держалось бы лишь на удержанном прежнем кадре.
//!
//! Число таких смен не меньше числа переездов, которые ждёт строка (по загруженным поколениям): без этого
//! старый плеер без строки (или строка, потерявшая смену) прошёл бы проверку вхолостую. Ввода-вывода тут
//! нет — каждое нарушение проверяется юнит-тестом.

use crate::observe::{MediaSwitch, parse_media_switch};

/// Предельное время от смены ролика до его первого кадра, мс. Удержание прежнего кадра (по умолчанию 3 с)
/// скрывает ожидание от зрителя, но не должно его оправдывать: ролик, который встал позже 2 с, — это уже
/// не «сразу после переезда». Предел строже срока удержания, чтобы долгое ожидание не маскировалось им.
pub(super) const MAX_FIRST_FRAME_AFTER_SWITCH_MS: u64 = 2000;

/// Нарушения правила по строкам журнала смен медиа. `expected_switches` — сколько переездов ждёт строка
/// (число загрузок поколений без первой).
pub(super) fn media_switch_failures(
    switch_lines: &[String],
    expected_switches: usize,
    failures: &mut Vec<String>,
) {
    let mut switches: Vec<MediaSwitch> = Vec::new();
    for line in switch_lines {
        match parse_media_switch(line) {
            Some(Ok(parsed)) => switches.push(parsed),
            // Незнакомое значение — провал: молча пропустить его значило бы не заметить новый исход плеера.
            Some(Err(reason)) => failures.push(format!(
                "the player wrote a media switch line the bench cannot read: {reason}"
            )),
            None => {}
        }
    }
    let completed: Vec<&MediaSwitch> = switches
        .iter()
        .filter(|candidate| candidate.is_completed_switch())
        .collect();
    for switch in &completed {
        completed_switch_failures(switch, failures);
    }
    if completed.len() < expected_switches {
        let not_completed: Vec<String> = switches
            .iter()
            .filter(|candidate| !candidate.is_completed_switch())
            .map(|candidate| {
                format!(
                    "switch {} ended {:?} ({:?})",
                    candidate.switch, candidate.end, candidate.outcome
                )
            })
            .collect();
        failures.push(format!(
            "the player log has {} finished media switch line(s) (end=first_frame), the row needs {expected_switches} \
             (one per relocation): no measurement for the missing ones{}",
            completed.len(),
            if not_completed.is_empty() {
                String::new()
            } else {
                format!("; other switch lines: {}", not_completed.join(", "))
            }
        ));
    }
}

/// Нарушения одной завершённой смены: пустые кадры и слишком поздний первый кадр.
fn completed_switch_failures(switch: &MediaSwitch, failures: &mut Vec<String>) {
    if switch.empty_frames != 0 {
        failures.push(format!(
            "media switch {}: {} empty frame(s) (no video) reached the screen before the first frame of the new reel \
             (outcome {:?}, release {:?}, held {} frame(s)): {}",
            switch.switch,
            switch.empty_frames,
            switch.outcome,
            switch.release,
            switch.held_frames,
            switch.line
        ));
    }
    match switch.first_frame_ms {
        Some(elapsed) if elapsed <= MAX_FIRST_FRAME_AFTER_SWITCH_MS => {}
        Some(elapsed) => failures.push(format!(
            "media switch {}: the first frame of the new reel came after {elapsed} ms, the limit is \
             {MAX_FIRST_FRAME_AFTER_SWITCH_MS} ms: {}",
            switch.switch, switch.line
        )),
        None => failures.push(format!(
            "media switch {}: the line says the switch ended with a first frame but gives no first_frame_ms: {}",
            switch.switch, switch.line
        )),
    }
}

/// Здоровые строки для тестов соседних приговоров: первый ролик окна и пять чистых смен (запас, чтобы
/// тесты, меняющие число поколений, не упирались в нехватку строк).
#[cfg(test)]
pub(super) fn healthy_switch_lines() -> Vec<String> {
    let mut lines = vec![switch_line(
        1,
        "first_frame",
        "no_previous_frame",
        0,
        Some(90),
    )];
    lines.extend((2..=6).map(|number| switch_line(number, "first_frame", "held", 0, Some(140))));
    lines
}

/// Строка смены в формате плеера (для тестов).
#[cfg(test)]
pub(super) fn switch_line(
    number: u64,
    end: &str,
    outcome: &str,
    empty_frames: u64,
    first_frame_ms: Option<u64>,
) -> String {
    let first_frame = first_frame_ms.map_or("none".to_owned(), |millis| millis.to_string());
    format!(
        "2026-11-20T10:00:00Z  INFO fastiplayer: Browser window media switch switch={number} end={end} \
         outcome={outcome} release=none empty_frames={empty_frames} held_frames=2 \
         first_frame_ms={first_frame} render_generation={number}"
    )
}

#[cfg(test)]
mod tests;
