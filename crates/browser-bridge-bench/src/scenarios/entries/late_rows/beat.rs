//! Пульс страницы (`origin_beat=1`, `site/lib/entry-probe.js`): по нему робот видит, перезагружали
//! ли вкладку и с какой секунды ролика это случилось.
//!
//! Строка журнала: `<страница>: entry-probe origin <timeOrigin> videos=<имя>:<секунды>:<пауза 0|1>;...`.
//! `timeOrigin` — момент рождения документа: у перезагруженной страницы он другой.

/// Метка строки пульса в журнале страницы.
pub(super) const BEAT_LINE_MARK: &str = "entry-probe origin ";

/// Позиция одного `<video>` страницы в момент пульса.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct VideoBeat {
    pub(super) name: String,
    pub(super) seconds: f64,
    pub(super) paused: bool,
}

/// Один пульс: когда родилась страница и где стоят её видео.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Beat {
    /// `performance.timeOrigin` текстом: сравниваем на равенство, число не нужно.
    pub(super) origin: String,
    pub(super) videos: Vec<VideoBeat>,
}

impl Beat {
    /// Видео с этим именем (`id` элемента), если оно есть на странице.
    pub(super) fn video(&self, name: &str) -> Option<&VideoBeat> {
        self.videos.iter().find(|video| video.name == name)
    }
}

/// Разбирает строку журнала в пульс; неразборчивая строка — ошибка, а не пропуск.
pub(super) fn parse_beat_line(line: &str) -> Result<Beat, String> {
    let (_, rest) = line
        .split_once(BEAT_LINE_MARK)
        .ok_or_else(|| format!("not a beat line: {line}"))?;
    let (origin, videos_text) = rest
        .split_once(" videos=")
        .ok_or_else(|| format!("beat line without videos: {line}"))?;
    let mut videos = Vec::new();
    for item in videos_text.split(';').filter(|item| !item.is_empty()) {
        // Имя может быть любым id, поэтому число и флаг отрезаем с конца.
        let mut parts = item.rsplitn(3, ':');
        let (paused, seconds, name) = (parts.next(), parts.next(), parts.next());
        let (Some(paused), Some(seconds), Some(name)) = (paused, seconds, name) else {
            return Err(format!("bad video item `{item}` in: {line}"));
        };
        videos.push(VideoBeat {
            name: name.to_owned(),
            seconds: seconds
                .parse()
                .map_err(|_| format!("bad seconds `{seconds}` in: {line}"))?,
            paused: match paused {
                "0" => false,
                "1" => true,
                other => return Err(format!("bad pause flag `{other}` in: {line}")),
            },
        });
    }
    Ok(Beat {
        origin: origin.trim().to_owned(),
        videos,
    })
}

/// Все пульсы прогона страницы в порядке прихода.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct PageBeats {
    beats: Vec<Beat>,
}

impl PageBeats {
    /// Из строк журнала страницы (`page_log_lines(BEAT_LINE_MARK)`).
    pub(super) fn from_log(lines: &[String]) -> Result<Self, String> {
        Ok(Self {
            beats: lines
                .iter()
                .map(|line| parse_beat_line(line))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Сколько пульсов пришло.
    pub(super) fn count(&self) -> usize {
        self.beats.len()
    }

    /// Разные моменты рождения страницы по порядку появления: один — вкладку не перезагружали.
    pub(super) fn origins(&self) -> Vec<&str> {
        let mut origins: Vec<&str> = Vec::new();
        for beat in &self.beats {
            if !origins.contains(&beat.origin.as_str()) {
                origins.push(&beat.origin);
            }
        }
        origins
    }

    /// Последний пульс.
    pub(super) fn latest(&self) -> Option<&Beat> {
        self.beats.last()
    }

    /// Последняя позиция видео `name` у САМОГО ПЕРВОГО рождения страницы, то есть секунда, на которой
    /// ролик стоял к перезагрузке (или к «сейчас», если страницу не перезагружали).
    pub(super) fn last_seconds_of_first_page(&self, name: &str) -> Option<f64> {
        let first_origin = self.beats.first()?.origin.as_str();
        self.beats
            .iter()
            .take_while(|beat| beat.origin == first_origin)
            .filter_map(|beat| beat.video(name))
            .last()
            .map(|video| video.seconds)
    }

    /// Последний пульс страницы, родившейся ПОСЛЕ перезагрузки (с другим моментом рождения).
    pub(super) fn latest_of_reloaded_page(&self) -> Option<&Beat> {
        let first_origin = self.beats.first()?.origin.as_str();
        self.beats
            .iter()
            .rev()
            .find(|beat| beat.origin != first_origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(origin: &str, videos: &str) -> String {
        format!("mse-fmp4: entry-probe origin {origin} videos={videos}")
    }

    #[test]
    fn a_beat_line_gives_the_origin_and_every_video() {
        let beat = parse_beat_line(&line("1730.5", "ad:3.20:0;film:12.50:1")).expect("parses");
        assert_eq!(beat.origin, "1730.5");
        assert_eq!(beat.video("ad").map(|video| video.seconds), Some(3.2));
        let film = beat.video("film").expect("film");
        assert_eq!((film.seconds, film.paused), (12.5, true));
        assert!(beat.video("missing").is_none());
        assert!(
            parse_beat_line(&line("1.0", ""))
                .expect("a page without videos")
                .videos
                .is_empty()
        );
    }

    #[test]
    fn broken_lines_are_errors_not_zeros() {
        for broken in [
            "mse-fmp4: something else",
            "mse-fmp4: entry-probe origin 1.0",
            "x: entry-probe origin 1.0 videos=film:abc:0",
            "x: entry-probe origin 1.0 videos=film:1.0:2",
            "x: entry-probe origin 1.0 videos=film",
        ] {
            assert!(parse_beat_line(broken).is_err(), "{broken}");
        }
    }

    #[test]
    fn a_reload_shows_as_a_second_origin_and_keeps_the_last_second_before_it() {
        let lines = vec![
            line("100", "v:1.00:0"),
            line("100", "v:6.50:0"),
            line("200", "v:0.10:0"),
            line("200", "v:2.00:0"),
        ];
        let beats = PageBeats::from_log(&lines).expect("parses");
        assert_eq!(beats.origins(), vec!["100", "200"]);
        assert_eq!(beats.last_seconds_of_first_page("v"), Some(6.5));
        assert_eq!(
            beats
                .latest_of_reloaded_page()
                .and_then(|beat| beat.video("v"))
                .map(|video| video.seconds),
            Some(2.0)
        );
        assert_eq!(beats.count(), 4);
    }

    #[test]
    fn a_page_that_was_never_reloaded_has_one_origin_and_no_reloaded_page() {
        let beats =
            PageBeats::from_log(&[line("100", "v:1.00:0"), line("100", "v:2.00:0")]).unwrap();
        assert_eq!(beats.origins(), vec!["100"]);
        assert!(beats.latest_of_reloaded_page().is_none());
        assert_eq!(beats.last_seconds_of_first_page("v"), Some(2.0));
        assert_eq!(PageBeats::default().last_seconds_of_first_page("v"), None);
    }
}
