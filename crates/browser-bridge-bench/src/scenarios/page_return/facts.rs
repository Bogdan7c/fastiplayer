//! Факты сценария `page-return` из журналов: строки страницы (`site/lib/return-checks.js`) и строки
//! плеера о конце сеанса. Здесь только разбор; решения — в [`super::judge`].

/// Метка строки страницы «секунда прямо перед действием».
pub(super) const RETURN_MARK_LINE: &str = "return-mark ";
/// Метка строки страницы с фактами возврата.
pub(super) const RETURN_FACTS_LINE: &str = "return-facts ";
/// Метка строки страницы с лентой событий от отметки (только для сводки и разбора провала).
pub(super) const RETURN_TIMELINE_LINE: &str = "return-timeline ";
/// Метка строки страницы со счётом кликов, дошедших до `<video>`.
pub(super) const VIDEO_CLICKS_LINE: &str = "video-clicks ";

/// Строка плеера о конце сеанса (`SessionLifecycleEvent::Ended`, поле `reason` в отладочном виде).
pub(super) const SESSION_ENDED_LINE: &str = "Browser session ended";
/// Причина конца «зритель нажал Return» в этой строке (код 4010 `viewer_returned_to_page`).
pub(super) const VIEWER_RETURNED_REASON: &str = "ClosedByPage { code: Some(4010) }";
/// Плеер попрощался со страницей (`return_to_page`): так кончается закрытие окна человеком.
pub(super) const FAREWELL_LINE: &str = "Session closed with a farewell to the page";
/// Плеер открыл сеанс (каждая передача — одна такая строка).
pub(super) const SESSION_OPENED_LINE: &str = "Browser session opened";
/// Доигрывание скачанного кончилось.
pub(super) const DRAIN_FINISHED_LINE: &str = "Playback drain finished";
/// Процесс браузерного режима вышел (вместе с ним ушло и окно).
pub(super) const PROCESS_FINISHED_LINE: &str = "Browser session process finished";
/// Строки, которых не должно быть после «Return» (решение 236: окно закрывается сразу и молча) —
/// тот же список, что у процессного теста плеера `SILENT_WINDOW_END`.
pub(super) const SILENT_END_FORBIDDEN: &[&str] = &[
    DRAIN_FINISHED_LINE,
    FAREWELL_LINE,
    "Browser session end notice shown",
    "Browser window shows the end notice before closing",
];

/// Что страница видела от отметки до конца наблюдения (строка `return-facts`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ReturnFacts {
    /// Поворот был: браузер снова декодирует сам или элемент встал на паузу.
    pub(super) turned: bool,
    /// Секунда страницы в миг отметки (прямо перед действием робота).
    pub(super) mark_s: f64,
    /// Секунда страницы в миг поворота.
    pub(super) turn_s: f64,
    /// Сколько прошло от отметки до поворота, с.
    pub(super) elapsed_s: f64,
    /// Секунда страницы в конце наблюдения.
    pub(super) final_s: f64,
    /// Элемент на паузе в конце наблюдения.
    pub(super) final_paused: bool,
    /// Длина наблюдения после поворота, с.
    pub(super) settle_s: f64,
    /// Сколько настоящих кадров браузер декодировал за наблюдение.
    pub(super) settle_frames: u64,
    /// Сколько раз `paused` сменился с отметки до конца.
    pub(super) paused_flips: u32,
    /// Самый большой скачок часов против хода стенных часов, с.
    pub(super) max_jump_s: f64,
    pub(super) play_events: u32,
    pub(super) pause_events: u32,
    pub(super) seeking_events: u32,
}

/// Значение поля `name=` (до пробела); левая граница — начало строки или пробел.
fn field<'line>(line: &'line str, name: &str) -> Option<&'line str> {
    let pattern = format!("{name}=");
    let (start, _) = line
        .match_indices(&pattern)
        .find(|(index, _)| *index == 0 || line.as_bytes()[index - 1] == b' ')?;
    let rest = &line[start + pattern.len()..];
    rest.split_whitespace().next()
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

/// Разбирает `return-facts …`. Строка без поля — ошибка, а не нули: иначе провал прятался бы.
pub(super) fn parse_return_facts(line: &str) -> Result<ReturnFacts, String> {
    let (_, body) = line
        .split_once(RETURN_FACTS_LINE)
        .ok_or_else(|| format!("not a return-facts line: {line}"))?;
    Ok(ReturnFacts {
        turned: flag(body, "turned")?,
        mark_s: number(body, "mark_s")?,
        turn_s: number(body, "turn_s")?,
        elapsed_s: number(body, "elapsed_s")?,
        final_s: number(body, "final_s")?,
        final_paused: flag(body, "final_paused")?,
        settle_s: number(body, "settle_s")?,
        settle_frames: number(body, "settle_frames")?,
        paused_flips: number(body, "paused_flips")?,
        max_jump_s: number(body, "max_jump_s")?,
        play_events: number(body, "play_events")?,
        pause_events: number(body, "pause_events")?,
        seeking_events: number(body, "seeking_events")?,
    })
}

/// Разбирает `return-mark t=<с> …` — секунду страницы в миг отметки.
pub(super) fn parse_return_mark(line: &str) -> Result<f64, String> {
    let (_, body) = line
        .split_once(RETURN_MARK_LINE)
        .ok_or_else(|| format!("not a return-mark line: {line}"))?;
    number(body, "t")
}

/// Разбирает `video-clicks n=<число>`; `-1` значит «счёт не включали».
pub(super) fn parse_video_clicks(line: &str) -> Result<i64, String> {
    let (_, body) = line
        .split_once(VIDEO_CLICKS_LINE)
        .ok_or_else(|| format!("not a video-clicks line: {line}"))?;
    number(body, "n")
}

/// Что журнал плеера говорит о конце сеанса.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct PlayerEndFacts {
    /// Строки «Browser session ended» (по одной на сеанс).
    pub(super) ended_lines: Vec<String>,
    /// Строки, которых после «Return» быть не должно (доигрывание, прощание, сообщение).
    pub(super) silent_end_violations: Vec<String>,
    /// Сколько сеансов плеер открыл.
    pub(super) sessions_opened: usize,
    /// Плеер попрощался со страницей (`return_to_page`).
    pub(super) farewell: bool,
}

impl PlayerEndFacts {
    /// Собирает факты из всех строк журнала плеера за прогон.
    pub(super) fn from_lines(lines: &[String]) -> Self {
        let containing = |needle: &str| -> Vec<String> {
            lines
                .iter()
                .filter(|line| line.contains(needle))
                .cloned()
                .collect()
        };
        Self {
            ended_lines: containing(SESSION_ENDED_LINE),
            silent_end_violations: lines
                .iter()
                .filter(|line| {
                    SILENT_END_FORBIDDEN
                        .iter()
                        .any(|needle| line.contains(needle))
                })
                .cloned()
                .collect(),
            sessions_opened: containing(SESSION_OPENED_LINE).len(),
            farewell: !containing(FAREWELL_LINE).is_empty(),
        }
    }

    /// Хоть один сеанс кончился кодом 4010 «зритель вернулся к странице».
    pub(super) fn ended_by_viewer_return(&self) -> bool {
        self.ended_lines
            .iter()
            .any(|line| line.contains(VIEWER_RETURNED_REASON))
    }
}

/// Все строки журнала плеера, которые нужны фактам конца (для `PlayerLog::snapshot_forbidden`).
pub(super) const PLAYER_END_NEEDLES: &[&str] = &[
    SESSION_ENDED_LINE,
    SESSION_OPENED_LINE,
    DRAIN_FINISHED_LINE,
    FAREWELL_LINE,
    "Browser session end notice shown",
    "Browser window shows the end notice before closing",
];

#[cfg(test)]
mod tests {
    use super::*;

    const FACTS: &str = "return-kit: return-facts turned=1 mark_s=12.400 turn_s=13.100 elapsed_s=0.800 final_s=16.050 final_paused=0 settle_s=3.010 settle_frames=74 paused_flips=0 max_jump_s=0.120 play_events=1 pause_events=0 seeking_events=1";

    #[test]
    fn a_facts_line_gives_every_number() {
        let facts = parse_return_facts(FACTS).expect("parses");
        assert!(facts.turned && !facts.final_paused);
        assert_eq!(facts.mark_s, 12.4);
        assert_eq!(facts.turn_s, 13.1);
        assert_eq!(facts.elapsed_s, 0.8);
        assert_eq!(facts.final_s, 16.05);
        assert_eq!(facts.settle_frames, 74);
        assert_eq!(facts.paused_flips, 0);
        assert_eq!(facts.max_jump_s, 0.12);
        assert_eq!(
            (facts.play_events, facts.pause_events, facts.seeking_events),
            (1, 0, 1)
        );
    }

    #[test]
    fn a_facts_line_without_a_field_or_with_garbage_is_an_error() {
        assert!(parse_return_facts(&FACTS.replace(" paused_flips=0", "")).is_err());
        assert!(parse_return_facts(&FACTS.replace("final_paused=0", "final_paused=yes")).is_err());
        assert!(parse_return_facts(&FACTS.replace("mark_s=12.400", "mark_s=NaNx")).is_err());
        assert!(parse_return_facts("hello").is_err());
    }

    #[test]
    fn a_field_name_inside_another_name_is_not_taken() {
        // `turn_s` не должно читаться из хвоста другого имени (`return_s=`).
        let line = FACTS.replace("turn_s=13.100", "return_s=99 turn_s=13.100");
        assert_eq!(parse_return_facts(&line).expect("parses").turn_s, 13.1);
    }

    #[test]
    fn mark_and_click_lines_give_their_numbers() {
        assert_eq!(
            parse_return_mark("p: return-mark t=605.250 paused=0"),
            Ok(605.25)
        );
        assert!(parse_return_mark("p: return-mark paused=0").is_err());
        assert_eq!(parse_video_clicks("p: video-clicks n=0"), Ok(0));
        assert_eq!(parse_video_clicks("p: video-clicks n=-1"), Ok(-1));
        assert!(parse_video_clicks("p: video-clicks").is_err());
    }

    #[test]
    fn player_end_facts_tell_a_silent_return_from_a_farewell() {
        let silent = PlayerEndFacts::from_lines(&[
            "INFO Browser session opened intent=Window".to_owned(),
            "INFO Browser session ended reason=ClosedByPage { code: Some(4010) }".to_owned(),
            "INFO Browser session process finished".to_owned(),
        ]);
        assert!(silent.ended_by_viewer_return());
        assert!(silent.silent_end_violations.is_empty());
        assert_eq!(silent.sessions_opened, 1);
        assert!(!silent.farewell);

        let closed = PlayerEndFacts::from_lines(&[
            "INFO Browser session opened".to_owned(),
            "INFO Session closed with a farewell to the page position=12".to_owned(),
            "INFO Browser session ended reason=ClosedByPlayer".to_owned(),
        ]);
        assert!(!closed.ended_by_viewer_return());
        assert!(closed.farewell);
        assert_eq!(closed.silent_end_violations.len(), 1);
        // 4009 — тоже «закрыла страница», но не «Return».
        let took_back = PlayerEndFacts::from_lines(&[
            "INFO Browser session ended reason=ClosedByPage { code: Some(4009) }".to_owned(),
        ]);
        assert!(!took_back.ended_by_viewer_return());
    }
}
