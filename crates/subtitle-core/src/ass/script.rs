// Порт поведения libass 0.17.5 (ISC): ass_utils.c (ass_lookup_style), ass_parse.c (lookup_style_strict).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
// Copyright (C) 2009 Grigori Goronzy <greg@geekmind.org>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Сценарий ASS: заголовок, стили и раскладка событий. Один на дорожку.

use std::collections::HashMap;

use super::event_format::AssEventFormat;
use super::script_info::AssScriptInfo;
use super::style::AssStyle;

/// Версия формата сценария.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssScriptKind {
    /// ASS (`[V4+ Styles]`, `ScriptType: v4.00+`).
    Ass,
    /// SSA (`[V4 Styles]`, `ScriptType: v4.00`).
    Ssa,
}

/// Индекс стиля внутри [`AssScript`]. Получить его можно только из поиска по имени,
/// поэтому для «своего» сценария он всегда валиден. Индекс 0 — встроенный стиль libass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssStyleIndex(usize);

impl AssStyleIndex {
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Сценарий ASS. Неизменяем после создания и раздаётся событиям через `Arc`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssScript {
    kind: AssScriptKind,
    info: AssScriptInfo,
    event_format: AssEventFormat,
    /// Встроенный стиль libass (индекс 0).
    builtin_default_style: AssStyle,
    /// Стили из файла: индекс `i + 1` в [`AssStyleIndex`].
    file_styles: Vec<AssStyle>,
    /// Имя -> индекс (с учётом смещения на встроенный стиль) последнего стиля с этим именем.
    last_index_by_name: HashMap<String, usize>,
    default_style: AssStyleIndex,
}

impl AssScript {
    /// Собирает сценарий. Индекс 0 всегда занят встроенным стилем libass
    /// ([`AssStyle::libass_default`]); `file_styles` идут следом в порядке файла.
    ///
    /// Стиль по умолчанию — последний стиль с именем ровно `Default` (как в `process_style`),
    /// а если такого нет — встроенный.
    pub fn new(
        kind: AssScriptKind,
        info: AssScriptInfo,
        event_format: AssEventFormat,
        file_styles: Vec<AssStyle>,
    ) -> Self {
        let default_style = file_styles
            .iter()
            .rposition(|style| style.name == DEFAULT_STYLE_NAME)
            .map_or(AssStyleIndex(0), |position| AssStyleIndex(position + 1));
        // Имя -> индекс последнего стиля с этим именем (последний выигрывает, как поиск с
        // конца в libass). `lookup_style_strict` libass сравнивает имя целиком, поэтому
        // обычной карты достаточно, а поиск на каждое событие не зависит от числа стилей.
        let mut last_index_by_name = HashMap::with_capacity(file_styles.len());
        for (position, style) in file_styles.iter().enumerate() {
            last_index_by_name.insert(style.name.clone(), position + 1);
        }
        Self {
            kind,
            info,
            event_format,
            builtin_default_style: AssStyle::libass_default(),
            file_styles,
            last_index_by_name,
            default_style,
        }
    }

    pub fn kind(&self) -> AssScriptKind {
        self.kind
    }

    pub fn info(&self) -> &AssScriptInfo {
        &self.info
    }

    /// Раскладка полей `Dialogue:` для разбора событий.
    pub fn event_format(&self) -> &AssEventFormat {
        &self.event_format
    }

    /// Сколько стилей, включая встроенный.
    pub fn style_count(&self) -> usize {
        self.file_styles.len() + 1
    }

    /// Стиль по индексу. Индекс из чужого сценария (вне диапазона) даёт встроенный стиль:
    /// паниковать на данных из файла нельзя.
    pub fn style(&self, index: AssStyleIndex) -> &AssStyle {
        match index.0 {
            0 => &self.builtin_default_style,
            position => self
                .file_styles
                .get(position - 1)
                .unwrap_or(&self.builtin_default_style),
        }
    }

    /// Стиль, который используется, когда имя не найдено.
    pub fn default_style_index(&self) -> AssStyleIndex {
        self.default_style
    }

    /// Поиск стиля реплики по имени (`ass_lookup_style`): ведущие `*` отбрасываются,
    /// `default` в любом регистре превращается в `Default`, поиск идёт с последнего стиля
    /// к первому, а при промахе возвращается стиль по умолчанию.
    pub fn lookup_style(&self, name: &str) -> AssStyleIndex {
        let stripped = name.trim_start_matches('*');
        let wanted = if stripped.eq_ignore_ascii_case(DEFAULT_STYLE_NAME) {
            DEFAULT_STYLE_NAME
        } else {
            stripped
        };
        self.lookup_style_strict(wanted)
            .unwrap_or(self.default_style)
    }

    /// Точный поиск (`lookup_style_strict`, для тега `\r`): без нормализации, с последнего
    /// стиля к первому; промах — `None`.
    pub fn lookup_style_strict(&self, name: &str) -> Option<AssStyleIndex> {
        if let Some(&index) = self.last_index_by_name.get(name) {
            return Some(AssStyleIndex(index));
        }
        (self.builtin_default_style.name == name).then_some(AssStyleIndex(0))
    }
}

/// Имя встроенного и «главного» стиля.
const DEFAULT_STYLE_NAME: &str = "Default";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ass::{
        AssKerning, AssPlayRes, AssScaledBorderAndShadow, AssWrapStyle, AssYCbCrMatrix,
    };

    fn info() -> AssScriptInfo {
        AssScriptInfo {
            play_res: AssPlayRes::resolve(0, 0),
            layout_res: None,
            scaled_border_and_shadow: AssScaledBorderAndShadow::Disabled,
            wrap_style: AssWrapStyle::Smart,
            kerning: AssKerning::Disabled,
            ycbcr_matrix: AssYCbCrMatrix::Default,
            language: None,
        }
    }

    fn named(name: &str, font_size: f64) -> AssStyle {
        AssStyle {
            name: name.to_owned(),
            font_size,
            ..AssStyle::libass_default()
        }
    }

    fn build_script(styles: Vec<AssStyle>) -> AssScript {
        AssScript::new(
            AssScriptKind::Ass,
            info(),
            AssEventFormat::new(Vec::new()),
            styles,
        )
    }

    #[test]
    fn script_without_styles_resolves_everything_to_builtin() {
        let script = build_script(Vec::new());
        assert_eq!(script.style_count(), 1);
        let found = script.lookup_style("Anything");
        assert_eq!(found.get(), 0);
        assert_eq!(script.style(found).font_size, 18.0);
    }

    #[test]
    fn default_style_is_last_file_style_named_default() {
        let script = build_script(vec![
            named("Default", 20.0),
            named("Sign", 30.0),
            named("Default", 40.0),
        ]);
        assert_eq!(script.default_style_index().get(), 3);
        assert_eq!(script.style(script.default_style_index()).font_size, 40.0);
    }

    #[test]
    fn lookup_searches_from_last_style_backwards() {
        let script = build_script(vec![named("Top", 1.0), named("Top", 2.0)]);
        let found = script.lookup_style("Top");
        assert_eq!(found.get(), 2);
        assert_eq!(script.style(found).font_size, 2.0);
    }

    #[test]
    fn lookup_strips_stars_and_normalizes_default_case() {
        let script = build_script(vec![named("Default", 25.0), named("Sign", 30.0)]);
        assert_eq!(script.lookup_style("*Sign").get(), 2);
        assert_eq!(script.lookup_style("**Sign").get(), 2);
        assert_eq!(script.lookup_style("dEfAuLt").get(), 1);
        assert_eq!(script.lookup_style("*DEFAULT").get(), 1);
    }

    #[test]
    fn lookup_is_case_sensitive_for_other_names_and_miss_gives_default_style() {
        let script = build_script(vec![named("Sign", 30.0), named("Default", 25.0)]);
        // `sign` != `Sign`: промах -> стиль по умолчанию (индекс 2).
        assert_eq!(script.lookup_style("sign").get(), 2);
        assert_eq!(script.lookup_style("").get(), 2);
    }

    #[test]
    fn strict_lookup_has_no_normalization_and_reports_miss() {
        let script = build_script(vec![named("Default", 25.0), named("Sign", 30.0)]);
        assert_eq!(script.lookup_style_strict("Sign").map(|i| i.get()), Some(2));
        assert_eq!(script.lookup_style_strict("*Sign"), None);
        assert_eq!(script.lookup_style_strict("default"), None);
        assert_eq!(script.lookup_style_strict("Missing"), None);
        // Встроенный стиль тоже виден строгому поиску, если файл не переопределил имя.
        let bare = build_script(vec![named("Sign", 1.0)]);
        assert_eq!(
            bare.lookup_style_strict("Default").map(|i| i.get()),
            Some(0)
        );
    }

    #[test]
    fn lookup_cost_does_not_grow_with_the_number_of_styles() {
        let styles: Vec<AssStyle> = (0..4_000)
            .map(|number| named(&format!("Style{number}"), 1.0))
            .collect();
        let script = build_script(styles);
        let started = std::time::Instant::now();
        let mut checksum = 0usize;
        for round in 0..100_000usize {
            // Первый стиль и промах: для линейного поиска это худшие случаи.
            checksum += script.lookup_style("Style0").get();
            checksum += script
                .lookup_style(if round % 2 == 0 { "Nope" } else { "Style3999" })
                .get();
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "поиск стиля должен быть O(1) на событие"
        );
        // Style0 -> индекс 1; промах -> стиль по умолчанию (встроенный, 0); Style3999 -> 4000.
        assert_eq!(checksum, 100_000 + 50_000 * 4_000);
    }

    #[test]
    fn foreign_index_falls_back_to_builtin_instead_of_panicking() {
        let big = build_script(vec![named("A", 1.0), named("B", 2.0)]);
        let small = build_script(Vec::new());
        let foreign = big.lookup_style("B");
        assert_eq!(small.style(foreign).name, "Default");
    }
}
