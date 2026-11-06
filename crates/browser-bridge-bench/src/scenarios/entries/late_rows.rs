//! Строки «поздний вход» (10d): вкладка открыта РАНЬШЕ экстеншена, и человек входит через значок.
//!
//! Что доказывает матрица (настоящая мышь и клавиши в невидимом kwin, `hand_off_tab` стенд не зовёт):
//! - экстеншен, поставленный или обновлённый при уже играющей вкладке, не молчит «видео не найдено», а
//!   предлагает в попапе «Reload and open» (`data-notice="reload_needed"`, кнопка `reload-and-open`);
//! - сама страница без нажатия кнопки НЕ перезагружается (человек мог что-то вводить);
//! - после кнопки вкладка перезагружается, а видео уходит в плеер с той же секунды;
//! - особые случаи: страница не запускает видео сама, сайт отбрасывает перемотку, перед фильмом идёт
//!   реклама.
//!
//! Архитектура. Данные строки описывает [`reload_row::ReloadPlan`] (какая страница, когда ставится
//! экстеншен, что делать после перезагрузки, какой судья), ход — [`reload_row::run`]. Движения
//! робота лежат в `steps` (попап, кнопка, пульс страницы), пульс страницы разбирает `beat`, частная
//! копия сборки для обновления — `extension_copy`. Строка с введённым текстом (без нажатия кнопки)
//! устроена иначе и живёт в `typed_text`.

mod beat;
mod extension_copy;
mod reload_row;
mod steps;
mod typed_text;

use std::time::Duration;

use super::super::context::{ScenarioError, Stand};
use super::super::outcome::Verdict;
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use reload_row::{AfterReload, ExtensionTiming, Judge, ReloadPlan, StartRule, Warmup};

/// Сколько страница играет сама, прежде чем появляется экстеншен (T из задания).
const PLAY_BEFORE_INSTALL: Duration = Duration::from_secs(6);
/// Дольше, чтобы запомненная секунда заметно отличалась от «с начала» (строка сайта-отказника).
const LONG_PLAY_BEFORE_INSTALL: Duration = Duration::from_secs(12);
/// До какой секунды фильма доигрывает страница с рекламой, прежде чем появляется экстеншен.
const FILM_SECONDS_BEFORE_INSTALL: f64 = 8.0;

/// Строка позднего входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LateRow {
    /// Экстеншен ставят при играющей вкладке.
    LateInstall,
    /// Экстеншен стоял, при играющей вкладке его обновляют.
    LateUpdate,
    /// Введённый текст: кнопку не нажимают, перезагрузки быть не должно.
    TypedText,
    /// Страница без автозапуска: человек сам нажал play до установки и после перезагрузки.
    PressPlay,
    /// Сайт возвращает ролик в начало при любой перемотке.
    SiteRefusesSecond,
    /// Каждая загрузка начинается с рекламы, фильм идёт в другом элементе.
    AdThenFilm,
}

impl LateRow {
    /// Все строки по порядку задания (сверяется с таблицей `ROWS`).
    #[cfg(test)]
    pub(super) const ALL: &'static [LateRow] = &[
        Self::LateInstall,
        Self::LateUpdate,
        Self::TypedText,
        Self::PressPlay,
        Self::SiteRefusesSecond,
        Self::AdThenFilm,
    ];

    /// Страница стенда строки.
    pub(super) const fn page_id(self) -> &'static str {
        match self {
            Self::LateInstall | Self::LateUpdate => "mse-fmp4",
            Self::TypedText => "late-typed-text",
            Self::PressPlay => "late-manual-play",
            Self::SiteRefusesSecond => "late-site-refuses-seek",
            Self::AdThenFilm => "late-ad-then-film",
        }
    }

    /// Правило строки в подписи таблицы.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::LateInstall => "late-install",
            Self::LateUpdate => "late-update",
            Self::TypedText => "late-typed-text-no-reload",
            Self::PressPlay => "late-press-play",
            Self::SiteRefusesSecond => "late-site-refuses-second",
            Self::AdThenFilm => "late-ad-then-film",
        }
    }

    /// План строки с перезагрузкой; у строки с текстом перезагрузки нет.
    fn reload_plan(self) -> Option<ReloadPlan> {
        let page_id = self.page_id();
        Some(match self {
            Self::LateInstall => ReloadPlan {
                page_id,
                main_video: "v",
                timing: ExtensionTiming::InstallLate,
                warmup: Warmup::Play(PLAY_BEFORE_INSTALL),
                after_reload: AfterReload::Straight,
                banner: None,
                start_rule: StartRule::Remembered,
                judge: Judge::Capture,
            },
            Self::LateUpdate => ReloadPlan {
                timing: ExtensionTiming::UpdateLate,
                ..Self::LateInstall.reload_plan()?
            },
            Self::TypedText => return None,
            Self::PressPlay => ReloadPlan {
                page_id,
                main_video: "film",
                timing: ExtensionTiming::InstallLate,
                warmup: Warmup::PressPlayThenPlay(PLAY_BEFORE_INSTALL),
                after_reload: AfterReload::PressPlay,
                banner: Some("opened"),
                start_rule: StartRule::Remembered,
                judge: Judge::Entry,
            },
            Self::SiteRefusesSecond => ReloadPlan {
                page_id,
                main_video: "film",
                timing: ExtensionTiming::InstallLate,
                warmup: Warmup::Play(LONG_PLAY_BEFORE_INSTALL),
                after_reload: AfterReload::Straight,
                banner: Some("opened_from_site_position"),
                start_rule: StartRule::SiteStart,
                judge: Judge::Entry,
            },
            Self::AdThenFilm => ReloadPlan {
                page_id,
                main_video: "film",
                timing: ExtensionTiming::InstallLate,
                warmup: Warmup::FilmReaches(FILM_SECONDS_BEFORE_INSTALL),
                after_reload: AfterReload::AdBeforeFilm,
                banner: None,
                start_rule: StartRule::Remembered,
                judge: Judge::Entry,
            },
        })
    }
}

/// Прогоняет строку в готовом свежем окружении.
pub(super) fn run(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: LateRow,
) -> Result<Verdict, ScenarioError> {
    match row.reload_plan() {
        Some(plan) => reload_row::run(stand, environment, browser, &plan),
        None => typed_text::run(stand, environment, browser, row.page_id()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    #[test]
    fn every_late_row_uses_a_known_stand_page_and_has_its_own_name() {
        let mut names = std::collections::HashSet::new();
        for row in LateRow::ALL {
            assert!(
                PAGE_SCENARIOS.iter().any(|page| page.id == row.page_id()),
                "{:?} points at an unknown page",
                row
            );
            assert!(names.insert(row.name()), "duplicate name {}", row.name());
        }
        assert_eq!(LateRow::ALL.len(), 6);
    }

    #[test]
    fn the_update_row_differs_from_the_install_row_only_in_timing() {
        let install = LateRow::LateInstall.reload_plan().expect("plan");
        let update = LateRow::LateUpdate.reload_plan().expect("plan");
        assert_eq!(update.timing, ExtensionTiming::UpdateLate);
        assert_eq!(install.timing, ExtensionTiming::InstallLate);
        assert_eq!(update.page_id, install.page_id);
        assert_eq!(update.main_video, install.main_video);
        assert!(LateRow::TypedText.reload_plan().is_none());
    }

    #[test]
    fn every_page_the_rows_script_declares_the_hooks_and_targets_the_robot_uses() {
        let source = |page: &str| {
            std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("site/pages")
                    .join(format!("{page}.html")),
            )
            .expect("page file")
        };
        let typed = source("late-typed-text");
        assert!(typed.contains(&format!("'{}'", typed_text::REPORT_STATE_HOOK)));
        assert!(typed.contains(&format!("'{}'", typed_text::INPUT_TARGET)));
        assert!(typed.contains(typed_text::STATE_LINE_MARK));
        let manual = source("late-manual-play");
        assert!(manual.contains(&format!("'{}'", reload_row::PLAY_BUTTON_TARGET)));
        // Видео, чью позицию робот читает по пульсу, называют именно так.
        assert!(source("late-ad-then-film").contains("id: 'ad'"));
        assert!(source("late-ad-then-film").contains("id: 'film'"));
        assert!(source("late-site-refuses-seek").contains("id: 'film'"));
    }
}
