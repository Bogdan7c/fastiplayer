//! Меню вариантов от адаптера сайта: озвучки, качества, субтитры.
//!
//! Адаптер читает список плеера страницы (решения 17, 19, 20), плеер
//! показывает его в своём меню (♪, качество, CC), а выбор возвращается
//! командой `select_variant` — переключает уже плеер страницы.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::ValueViolation;
use crate::limits::MAX_VARIANT_OPTIONS;
use crate::text::{VariantId, VariantLabel};

/// Какое меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantKind {
    /// Озвучки и дорожки звука.
    Audio,
    /// Качества видео.
    Quality,
    /// Субтитры.
    Subtitles,
}

/// Отмечен ли вариант как текущий на странице.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantSelection {
    /// Сейчас выбран на странице.
    Selected,
    /// Не выбран.
    Available,
}

/// Один вариант меню.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantOption {
    /// Идентификатор, который вернётся в `select_variant`.
    pub id: VariantId,
    /// Подпись для человека.
    pub label: VariantLabel,
    /// Выбран ли сейчас.
    pub selection: VariantSelection,
}

/// Меню целиком. Новое меню того же вида заменяет прежнее.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantMenu {
    /// Какое меню.
    pub kind: VariantKind,
    /// Варианты в порядке страницы. Пустой список — меню пропало.
    pub options: Vec<VariantOption>,
}

impl VariantMenu {
    /// Не больше лимита вариантов, ID уникальны, выбрано не больше одного.
    ///
    /// # Errors
    /// Нарушенное правило.
    pub fn validate(&self) -> Result<(), ValueViolation> {
        if self.options.len() > MAX_VARIANT_OPTIONS {
            return Err(ValueViolation::TooManyItems);
        }
        let unique_ids: BTreeSet<&VariantId> =
            self.options.iter().map(|option| &option.id).collect();
        if unique_ids.len() != self.options.len() {
            return Err(ValueViolation::DuplicateId);
        }
        let selected_count = self
            .options
            .iter()
            .filter(|option| option.selection == VariantSelection::Selected)
            .count();
        if selected_count > 1 {
            return Err(ValueViolation::ConflictingSelection);
        }
        Ok(())
    }
}
