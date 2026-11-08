//! Чистые судьи `tab-return` (сессия 11b2): разбор строк страницы и решения по собранным числам.
//! Ввода-вывода нет, поэтому всё проверяется юнит-тестами без браузера.

use crate::scenarios::capture::max_real_frames;

/// Допуск «плеер продолжил с той же секунды», с (план 11b2: секунда плеера ± 2 с).
pub(super) const POSITION_TOLERANCE_S: f64 = 2.0;
/// Самое короткое окно жизни вернувшегося документа, на котором счётчик браузера что-то доказывает
/// (то же число, что у `capture`): строк состояния страница пишет одну в секунду.
pub(super) const MIN_SECOND_DOCUMENT_WINDOW_S: f64 = 6.0;

/// Строка состояния страницы `tab-return: state doc=… second_load=… ct=… paused=… real_frames=… probe=…`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PageState {
    pub(super) document: String,
    pub(super) second_load: bool,
    pub(super) current_time_s: f64,
    pub(super) real_frames: u64,
    pub(super) probe_available: bool,
}

/// Значение поля `name=` в строке страницы.
fn field<'line>(line: &'line str, name: &str) -> Option<&'line str> {
    let marker = format!(" {name}=");
    let start = line.find(&marker)? + marker.len();
    line[start..].split_whitespace().next()
}

/// Разбор строки состояния; чужая или битая строка — `None`.
pub(super) fn parse_state(line: &str) -> Option<PageState> {
    if !line.contains("tab-return: state ") {
        return None;
    }
    Some(PageState {
        document: field(line, "doc")?.to_owned(),
        second_load: field(line, "second_load")? == "true",
        current_time_s: field(line, "ct")?.parse().ok()?,
        real_frames: field(line, "real_frames")?.parse().ok()?,
        probe_available: field(line, "probe")? == "true",
    })
}

/// Плеер продолжил с секунды, на которой зритель просил вернуть страницу (`asked_s`), а не отпрыгнул:
/// за время открытия (`waited_s`) он играл скачанное, поэтому верхняя граница шире на это время.
pub(super) fn judge_return_position(
    asked_s: f64,
    back_s: f64,
    waited_s: f64,
) -> Result<String, String> {
    let lowest = asked_s - POSITION_TOLERANCE_S;
    let highest = asked_s + waited_s + POSITION_TOLERANCE_S;
    if back_s < lowest || back_s > highest {
        return Err(format!(
            "after the page came back the player is at {back_s:.1} s, expected {lowest:.1}..{highest:.1} s (asked at {asked_s:.1} s, reopening took {waited_s:.1} s)"
        ));
    }
    Ok(format!(
        "asked at {asked_s:.1} s, continued at {back_s:.1} s"
    ))
}

/// Вернувшийся документ под тенью почти не декодирует: настоящих кадров не больше потолка `capture`.
/// `states` — все строки состояния прогона; судятся только строки второго документа.
pub(super) fn judge_second_document_decoding(states: &[PageState]) -> Result<String, String> {
    let second: Vec<&PageState> = states.iter().filter(|state| state.second_load).collect();
    let (Some(first), Some(last)) = (second.first(), second.last()) else {
        return Err("the reopened page never reported its state".to_owned());
    };
    if second.iter().any(|state| state.document != first.document) {
        return Err(
            "more than one reopened document reported: the page was opened twice".to_owned(),
        );
    }
    if !last.probe_available {
        return Err("the reopened page could not read the real frame counter".to_owned());
    }
    // Одна строка в секунду: окно — число строк. Кадры считаются от загрузки документа.
    let window_s = second.len() as f64;
    if window_s < MIN_SECOND_DOCUMENT_WINDOW_S {
        return Err(format!(
            "the reopened page lived {window_s:.0} s, too short to judge its decoder (need {MIN_SECOND_DOCUMENT_WINDOW_S} s)"
        ));
    }
    let allowed = max_real_frames(window_s);
    if last.real_frames as f64 > allowed {
        return Err(format!(
            "the reopened page decoded {} frames in {window_s:.0} s (allowed {allowed:.0}): the browser plays it by itself",
            last.real_frames
        ));
    }
    Ok(format!(
        "reopened page: {} real frames in {window_s:.0} s, page clock {:.1} s",
        last.real_frames, last.current_time_s
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(second_load: bool, real_frames: u64) -> PageState {
        PageState {
            document: if second_load { "b" } else { "a" }.to_owned(),
            second_load,
            current_time_s: 12.0,
            real_frames,
            probe_available: true,
        }
    }

    #[test]
    fn a_state_line_is_parsed_and_foreign_lines_are_not() {
        let line = "tab-return: state doc=k3j second_load=true ct=12.40 paused=false real_frames=7 probe=true";
        assert_eq!(
            parse_state(line),
            Some(PageState {
                document: "k3j".to_owned(),
                second_load: true,
                current_time_s: 12.4,
                real_frames: 7,
                probe_available: true,
            })
        );
        assert_eq!(
            parse_state("tab-return: load doc=k3j second_load=true"),
            None
        );
        assert_eq!(
            parse_state("tab-return: state doc=k3j second_load=true ct=x"),
            None
        );
    }

    #[test]
    fn the_player_must_continue_near_the_asked_second() {
        assert!(
            judge_return_position(10.0, 13.0, 2.5).is_ok(),
            "played on while the page reopened"
        );
        assert!(
            judge_return_position(10.0, 8.5, 2.5).is_ok(),
            "a bit behind within the tolerance"
        );
        assert!(
            judge_return_position(10.0, 0.4, 2.5).is_err(),
            "jumped back to the page's start"
        );
        assert!(
            judge_return_position(10.0, 20.0, 2.5).is_err(),
            "jumped forward"
        );
    }

    #[test]
    fn the_reopened_document_is_judged_by_its_own_lines() {
        let mut states: Vec<PageState> = (0..5).map(|_| state(false, 400)).collect();
        states.extend((0..8).map(|index| state(true, index * 2)));
        assert!(
            judge_second_document_decoding(&states).is_ok(),
            "the first document's frames do not count"
        );
        let mut playing_by_itself = states.clone();
        playing_by_itself.push(state(true, 400));
        assert!(judge_second_document_decoding(&playing_by_itself).is_err());
        let too_short: Vec<PageState> = (0..3).map(|_| state(true, 0)).collect();
        assert!(judge_second_document_decoding(&too_short).is_err());
        assert!(
            judge_second_document_decoding(&states[..5]).is_err(),
            "no reopened document"
        );
        let mut twice = states.clone();
        twice.push(PageState {
            document: "c".to_owned(),
            ..state(true, 1)
        });
        assert!(judge_second_document_decoding(&twice).is_err());
    }
}
