//! Разбор строки журнала плеера «смена медиа в окне браузерного режима» (16b3c, решение 380).
//!
//! Плеер пишет одну строку уровня info на каждую смену ролика в окне, все поля всегда, в этом порядке:
//! `Browser window media switch switch=<n> end=<…> outcome=<…> release=<…> empty_frames=<n>
//! held_frames=<n> first_frame_ms=<n|none> render_generation=<n|none>`.
//!
//! Разбор СТРОГИЙ: незнакомое значение `end`/`outcome`/`release`, пропавшее или нечисловое поле — это
//! ошибка разбора, а не молчаливый пропуск. Иначе плеер, добавивший новый исход, незаметно выпал бы из
//! проверки «первый кадр без пустого кадра». Всё здесь — чистые функции над строками.

use super::player_log::word_field;

/// Метка строки «смена медиа в окне».
pub const MEDIA_SWITCH_MARKER: &str = "Browser window media switch";

/// Чем кончилась смена медиа (поле `end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchEnd {
    /// Пришёл первый кадр нового ролика: единственный «успешный» конец смены.
    FirstFrame,
    LoadRefused,
    LoadFailed,
    PlayerStopped,
    PlayerFailed,
    PlayerShutdown,
    PlayerFatalError,
    MediaReleased,
    MediaReset,
    AudioOnlyMedia,
}

/// Что окно делало с прежним кадром (поле `outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutcome {
    /// Прежний кадр удержан до первого кадра нового ролика.
    Held,
    /// Новый кадр не пришёл за предельный срок: удержание кончилось.
    Expired,
    /// Удержанный кадр отпущен раньше срока (см. [`SwitchRelease`]).
    Released,
    /// Удержание выключено настройкой.
    Disabled,
    /// Прежнего кадра не было: это первый ролик окна, а не смена.
    NoPreviousFrame,
}

/// Почему удержанный кадр отпущен раньше срока (поле `release`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchRelease {
    SurfaceLifecycleBreak,
    RenderFailure,
    WorkerRenderError,
    BackendSwap,
    RendererReplaced,
    /// Кадр не отпускали.
    None,
}

/// Одна разобранная строка смены медиа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSwitch {
    /// Порядковый номер смены в окне; `1` с `NoPreviousFrame` — первый ролик окна.
    pub switch: u64,
    pub end: SwitchEnd,
    pub outcome: SwitchOutcome,
    pub release: SwitchRelease,
    /// Кадры окна, ушедшие на экран БЕЗ видео во время смены («пустые»).
    pub empty_frames: u64,
    /// Кадры, показанные с удержанным прежним видео.
    pub held_frames: u64,
    /// Сколько миллисекунд до первого кадра нового ролика (`none` — кадра не было).
    pub first_frame_ms: Option<u64>,
    /// Поколение рендера (`none` — не назначено).
    pub render_generation: Option<u64>,
    /// Строка журнала целиком.
    pub line: String,
}

impl MediaSwitch {
    /// Первая строка окна (первый ролик, а не смена): прежнего кадра не было.
    pub fn is_first_media_of_window(&self) -> bool {
        self.outcome == SwitchOutcome::NoPreviousFrame
    }

    /// Смена, кончившаяся первым кадром нового ролика, — предмет проверки «без пустого кадра».
    pub fn is_completed_switch(&self) -> bool {
        self.end == SwitchEnd::FirstFrame && !self.is_first_media_of_window()
    }
}

/// Разбор строки смены медиа. `None` — строка не такая (метки нет); `Some(Err)` — метка есть, но строка
/// сломана или с незнакомым значением; `Some(Ok)` — разобрана.
pub fn parse_media_switch(line: &str) -> Option<Result<MediaSwitch, String>> {
    if !line.contains(MEDIA_SWITCH_MARKER) {
        return None;
    }
    Some(parse_fields(line).map_err(|reason| format!("{reason}: {line}")))
}

/// Разбор полей строки с меткой.
fn parse_fields(line: &str) -> Result<MediaSwitch, String> {
    let end = match required_word(line, "end")?.as_str() {
        "first_frame" => SwitchEnd::FirstFrame,
        "load_refused" => SwitchEnd::LoadRefused,
        "load_failed" => SwitchEnd::LoadFailed,
        "player_stopped" => SwitchEnd::PlayerStopped,
        "player_failed" => SwitchEnd::PlayerFailed,
        "player_shutdown" => SwitchEnd::PlayerShutdown,
        "player_fatal_error" => SwitchEnd::PlayerFatalError,
        "media_released" => SwitchEnd::MediaReleased,
        "media_reset" => SwitchEnd::MediaReset,
        "audio_only_media" => SwitchEnd::AudioOnlyMedia,
        other => return Err(format!("unknown media switch end '{other}'")),
    };
    let outcome = match required_word(line, "outcome")?.as_str() {
        "held" => SwitchOutcome::Held,
        "expired" => SwitchOutcome::Expired,
        "released" => SwitchOutcome::Released,
        "disabled" => SwitchOutcome::Disabled,
        "no_previous_frame" => SwitchOutcome::NoPreviousFrame,
        other => return Err(format!("unknown media switch outcome '{other}'")),
    };
    let release = match required_word(line, "release")?.as_str() {
        "surface_lifecycle_break" => SwitchRelease::SurfaceLifecycleBreak,
        "render_failure" => SwitchRelease::RenderFailure,
        "worker_render_error" => SwitchRelease::WorkerRenderError,
        "backend_swap" => SwitchRelease::BackendSwap,
        "renderer_replaced" => SwitchRelease::RendererReplaced,
        "none" => SwitchRelease::None,
        other => return Err(format!("unknown media switch release '{other}'")),
    };
    Ok(MediaSwitch {
        switch: required_number(line, "switch")?,
        end,
        outcome,
        release,
        empty_frames: required_number(line, "empty_frames")?,
        held_frames: required_number(line, "held_frames")?,
        first_frame_ms: number_or_none(line, "first_frame_ms")?,
        render_generation: number_or_none(line, "render_generation")?,
        line: line.to_owned(),
    })
}

/// Поле-слово, которое обязано быть.
fn required_word(line: &str, name: &str) -> Result<String, String> {
    word_field(line, name).ok_or_else(|| format!("media switch line has no '{name}' field"))
}

/// Поле-число, которое обязано быть числом.
fn required_number(line: &str, name: &str) -> Result<u64, String> {
    let word = required_word(line, name)?;
    word.parse()
        .map_err(|_| format!("media switch field '{name}' is not a number: '{word}'"))
}

/// Поле «число или `none`».
fn number_or_none(line: &str, name: &str) -> Result<Option<u64>, String> {
    let word = required_word(line, name)?;
    if word == "none" {
        return Ok(None);
    }
    word.parse().map(Some).map_err(|_| {
        format!("media switch field '{name}' is neither a number nor 'none': '{word}'")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN: &str = "2026-11-20T10:00:00Z  INFO fastiplayer: Browser window media switch switch=2 end=first_frame outcome=held release=none empty_frames=0 held_frames=3 first_frame_ms=140 render_generation=2";

    #[test]
    fn a_clean_switch_is_parsed_field_by_field() {
        let parsed = parse_media_switch(CLEAN).expect("marker").expect("valid");
        assert_eq!(parsed.switch, 2);
        assert_eq!(parsed.end, SwitchEnd::FirstFrame);
        assert_eq!(parsed.outcome, SwitchOutcome::Held);
        assert_eq!(parsed.release, SwitchRelease::None);
        assert_eq!((parsed.empty_frames, parsed.held_frames), (0, 3));
        assert_eq!(parsed.first_frame_ms, Some(140));
        assert_eq!(parsed.render_generation, Some(2));
        assert!(parsed.is_completed_switch());
    }

    #[test]
    fn none_values_and_the_first_media_of_the_window_are_parsed() {
        let line = "Browser window media switch switch=1 end=first_frame outcome=no_previous_frame release=none empty_frames=0 held_frames=0 first_frame_ms=none render_generation=none";
        let parsed = parse_media_switch(line).expect("marker").expect("valid");
        assert_eq!(parsed.first_frame_ms, None);
        assert_eq!(parsed.render_generation, None);
        assert!(parsed.is_first_media_of_window());
        assert!(!parsed.is_completed_switch());
    }

    #[test]
    fn a_failed_switch_is_not_a_completed_one() {
        let line = CLEAN.replace("end=first_frame", "end=load_failed");
        let parsed = parse_media_switch(&line).expect("marker").expect("valid");
        assert_eq!(parsed.end, SwitchEnd::LoadFailed);
        assert!(!parsed.is_completed_switch());
    }

    #[test]
    fn other_lines_are_not_media_switches() {
        assert_eq!(
            parse_media_switch("INFO Browser playback summary kind=final"),
            None
        );
    }

    #[test]
    fn unknown_values_and_broken_fields_are_parse_errors() {
        for (from, to) in [
            ("end=first_frame", "end=brand_new_end"),
            ("outcome=held", "outcome=teleported"),
            ("release=none", "release=gremlins"),
            ("empty_frames=0", "empty_frames=many"),
            ("first_frame_ms=140", "first_frame_ms=soon"),
            ("held_frames=3 ", ""),
        ] {
            let line = CLEAN.replace(from, to);
            let parsed = parse_media_switch(&line).expect("marker is present");
            assert!(parsed.is_err(), "{from} -> {to} must be a parse error");
        }
    }
}
