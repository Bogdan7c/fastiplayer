//! Факты сценария `auto-handoff` из журнала страницы (`site/lib/auto-handoff-kit.js`). Здесь только
//! разбор; решения — в [`super::judge`]. Строка без поля — ошибка, а не нули: иначе провал прятался бы.

/// Метка строки доклада об окне наблюдения.
pub(super) const WINDOW_LINE: &str = "auto-window ";
/// Метка строки мгновенного снимка видео.
pub(super) const STATE_LINE: &str = "auto-state ";
/// Строка журнала: кнопка Play получила настоящий клик.
pub(super) const PLAY_CLICKED_LINE: &str = "play-clicked";
/// Строка журнала: превью запустилось по наведению мыши.
pub(super) const PREVIEW_HOVER_LINE: &str = "preview-hover-started";
/// Строка журнала: кнопка Pause получила настоящий клик.
pub(super) const PAUSE_CLICKED_LINE: &str = "pause-clicked";

/// Подстрока, по которой ищется доклад окна номер `seq` (номер растёт с каждым докладом страницы,
/// а строки журнала приходят вразнобой: «первая попавшаяся» была бы чужой).
pub(super) fn window_needle(seq: u32) -> String {
    format!("{WINDOW_LINE}seq={seq} ")
}

/// То же для снимка состояния.
pub(super) fn state_needle(seq: u32) -> String {
    format!("{STATE_LINE}seq={seq} ")
}

/// Длина ролика глазами страницы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum PageDuration {
    Finite(f64),
    /// `duration === Infinity`: эфир или поток без известной длины.
    Infinite,
    /// `NaN`: метаданные ещё не загружены.
    Unknown,
}

/// Окно наблюдения страницы: что она видела между «отметить» и «доложить».
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct WindowFacts {
    /// Счётчик настоящих кадров (мимо тени) читался; иначе кадры ничего не доказывают.
    pub(super) probe_available: bool,
    pub(super) window_s: f64,
    /// Сколько кадров браузер декодировал сам за окно.
    pub(super) real_frames: u64,
    /// На сколько ушла вперёд секунда страницы.
    pub(super) clock_advance_s: f64,
    pub(super) start_s: f64,
    pub(super) end_s: f64,
    /// Элемент на паузе в конце окна.
    pub(super) paused: bool,
    pub(super) muted: bool,
    pub(super) ended: bool,
    /// Ролик зациклен (превью): за окно он мог пройти конец, и ход часов ничего не доказывает.
    pub(super) looping: bool,
    pub(super) duration: PageDuration,
}

/// Мгновенный снимок видео страницы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StateFacts {
    pub(super) time_s: f64,
    pub(super) paused: bool,
    pub(super) ended: bool,
}

/// Значение поля `name=` (до пробела); левая граница — начало строки или пробел.
fn field<'line>(line: &'line str, name: &str) -> Option<&'line str> {
    let pattern = format!("{name}=");
    let (start, _) = line
        .match_indices(&pattern)
        .find(|(index, _)| *index == 0 || line.as_bytes()[index - 1] == b' ')?;
    line[start + pattern.len()..].split_whitespace().next()
}

fn number<T: std::str::FromStr>(line: &str, name: &str) -> Result<T, String> {
    field(line, name)
        .ok_or_else(|| format!("no {name}= in the page line: {line}"))?
        .parse()
        .map_err(|_| format!("{name}= is not a number in the page line: {line}"))
}

fn flag(line: &str, name: &str) -> Result<bool, String> {
    match field(line, name) {
        Some("1") => Ok(true),
        Some("0") => Ok(false),
        other => Err(format!(
            "{name}={other:?} is not 0/1 in the page line: {line}"
        )),
    }
}

fn duration(line: &str) -> Result<PageDuration, String> {
    match field(line, "duration") {
        Some("inf") => Ok(PageDuration::Infinite),
        Some("nan") => Ok(PageDuration::Unknown),
        Some(_) => number(line, "duration").map(PageDuration::Finite),
        None => Err(format!("no duration= in the page line: {line}")),
    }
}

/// Тело строки после метки (`auto-window `, `auto-state `).
fn body_after<'line>(line: &'line str, mark: &str) -> Result<&'line str, String> {
    line.split_once(mark)
        .map(|(_, body)| body)
        .ok_or_else(|| format!("not an '{mark}' line: {line}"))
}

/// Разбирает `auto-window seq=… ok=1 …`. `ok=0` (окно не отмечали) — ошибка с причиной страницы.
pub(super) fn parse_window(line: &str) -> Result<WindowFacts, String> {
    let body = body_after(line, WINDOW_LINE)?;
    if !flag(body, "ok")? {
        return Err(format!(
            "the page has no window to report ({})",
            field(body, "reason").unwrap_or("no reason given")
        ));
    }
    Ok(WindowFacts {
        probe_available: flag(body, "probe")?,
        window_s: number(body, "window_s")?,
        real_frames: number(body, "real_frames")?,
        clock_advance_s: number(body, "clock_advance_s")?,
        start_s: number(body, "start_s")?,
        end_s: number(body, "end_s")?,
        paused: flag(body, "paused")?,
        muted: flag(body, "muted")?,
        ended: flag(body, "ended")?,
        looping: flag(body, "looping")?,
        duration: duration(body)?,
    })
}

/// Разбирает `auto-state seq=… t=… paused=… ended=…`.
pub(super) fn parse_state(line: &str) -> Result<StateFacts, String> {
    let body = body_after(line, STATE_LINE)?;
    Ok(StateFacts {
        time_s: number(body, "t")?,
        paused: flag(body, "paused")?,
        ended: flag(body, "ended")?,
    })
}

/// Нашлась ли в журнале строка с настоящим кликом по Play, который страница приняла как жест человека.
pub(super) fn has_user_click(play_lines: &[String]) -> bool {
    play_lines.iter().any(|line| line.contains("active=true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: &str = "auto-handoff: auto-window seq=2 ok=1 probe=1 window_s=12.004 real_frames=287 clock_advance_s=11.9 start_s=0.512 end_s=12.412 paused=0 muted=0 ended=0 looping=0 duration=60.000";

    #[test]
    fn a_window_line_gives_every_fact() {
        let facts = parse_window(WINDOW).expect("parses");
        assert!(facts.probe_available);
        assert_eq!(facts.real_frames, 287);
        assert!((facts.window_s - 12.004).abs() < 1e-9);
        assert!((facts.clock_advance_s - 11.9).abs() < 1e-9);
        assert!(!facts.paused && !facts.muted && !facts.ended && !facts.looping);
        assert_eq!(facts.duration, PageDuration::Finite(60.0));
    }

    #[test]
    fn infinity_and_nan_durations_are_words_not_numbers() {
        let infinite = WINDOW.replace("duration=60.000", "duration=inf");
        assert_eq!(
            parse_window(&infinite).expect("parses").duration,
            PageDuration::Infinite
        );
        let unknown = WINDOW.replace("duration=60.000", "duration=nan");
        assert_eq!(
            parse_window(&unknown).expect("parses").duration,
            PageDuration::Unknown
        );
    }

    #[test]
    fn a_window_the_page_never_marked_is_an_error_with_its_reason() {
        let problem = parse_window("auto-window seq=1 ok=0 reason=no-mark").expect_err("no window");
        assert!(problem.contains("no-mark"), "{problem}");
    }

    #[test]
    fn missing_or_garbled_fields_are_errors_not_zeros() {
        let no_frames = WINDOW.replace("real_frames=287 ", "");
        assert!(
            parse_window(&no_frames)
                .expect_err("missing")
                .contains("real_frames")
        );
        let bad_flag = WINDOW.replace("paused=0", "paused=maybe");
        assert!(parse_window(&bad_flag).is_err());
        assert!(parse_window("something else").is_err());
    }

    #[test]
    fn a_field_name_inside_another_name_is_not_confused() {
        // `start_s=` не должно читаться из `window_start_s=`, а `t=` — из `start_s=`.
        let state = parse_state("auto-state seq=1 t=7.250 paused=1 ended=0").expect("parses");
        assert_eq!(
            state,
            StateFacts {
                time_s: 7.25,
                paused: true,
                ended: false
            }
        );
        assert!(parse_state("auto-state seq=1 start_s=7.250 paused=1 ended=0").is_err());
    }

    #[test]
    fn needles_pin_the_exact_report_number() {
        assert_eq!(window_needle(2), "auto-window seq=2 ");
        assert!(WINDOW.contains(&window_needle(2)));
        assert!(!WINDOW.contains(&window_needle(1)));
        // `seq=1 ` не совпадает с `seq=12 `.
        assert!(!"auto-window seq=12 ok=1".contains(&window_needle(1)));
    }

    #[test]
    fn a_click_counts_only_when_the_page_saw_a_user_activation() {
        assert!(has_user_click(&[
            "film: play-clicked active=true".to_owned()
        ]));
        assert!(!has_user_click(&[
            "film: play-clicked active=false".to_owned()
        ]));
        assert!(!has_user_click(&[]));
    }
}
