//! Правила караоке готовой реплики и роль каждой её картинки.

use super::rule::{KaraokeRunRule, NextChange};
use crate::clip::ImagePaint;

/// Роль картинки реплики для стадии караоке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageRole {
    /// Картинка от караоке не зависит (тень, подложка, текст без эффекта).
    Static,
    /// Заливка участка с правилом `rule` (индекс в [`KaraokeStage`]).
    Fill { rule: usize },
    /// Обводка участка с правилом `rule`: у `\ko` скрыта до начала слога.
    Outline { rule: usize },
}

/// Стадия караоке реплики: правила участков и роли картинок (по одной на картинку реплики,
/// в том же порядке, что `RasterizedCue::bitmaps`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct KaraokeStage {
    rules: Vec<KaraokeRunRule>,
    roles: Vec<ImageRole>,
}

impl KaraokeStage {
    /// Сдвигает роли на `count` картинок вперёд: перед текстом реплики добавили `count`
    /// картинок (плашка пользовательского стиля), караоке они не касаются.
    pub(crate) fn with_leading_static_images(mut self, count: usize) -> Self {
        let mut roles = vec![ImageRole::Static; count];
        roles.append(&mut self.roles);
        self.roles = roles;
        self
    }

    /// Раскраска каждой картинки реплики на момент `elapsed_ms` (мс от начала реплики).
    /// `None` — часов нет: все слоги спеты.
    pub(crate) fn paints_at(&self, elapsed_ms: Option<i64>) -> Vec<ImagePaint> {
        let run_paints: Vec<_> = self
            .rules
            .iter()
            .map(|rule| rule.paint_at(elapsed_ms))
            .collect();
        self.roles
            .iter()
            .map(|role| match *role {
                ImageRole::Static => ImagePaint::AsStored,
                ImageRole::Fill { rule } => run_paints
                    .get(rule)
                    .map_or(ImagePaint::AsStored, |paint| paint.glyph),
                ImageRole::Outline { rule } => match run_paints.get(rule) {
                    Some(paint) if !paint.outline_visible => ImagePaint::Hidden,
                    _ => ImagePaint::AsStored,
                },
            })
            .collect()
    }

    /// Ближайшее изменение раскраски после момента `elapsed_ms`.
    pub(crate) fn next_change(&self, elapsed_ms: i64) -> NextChange {
        self.rules
            .iter()
            .map(|rule| rule.next_change(elapsed_ms))
            .fold(NextChange::Never, |earliest, change| {
                match (earliest, change) {
                    (NextChange::EveryFrame, _) | (_, NextChange::EveryFrame) => {
                        NextChange::EveryFrame
                    }
                    (NextChange::Never, other) | (other, NextChange::Never) => other,
                    (NextChange::AtMs(first), NextChange::AtMs(second)) => {
                        NextChange::AtMs(first.min(second))
                    }
                }
            })
    }
}

/// Накопитель правил при композиции: правила участков и роли картинок по мере их выдачи.
#[derive(Debug, Default)]
pub(crate) struct KaraokeStageBuilder {
    rules: Vec<KaraokeRunRule>,
    roles: Vec<ImageRole>,
}

impl KaraokeStageBuilder {
    /// Добавляет правило участка и возвращает его индекс для ролей.
    pub(crate) fn add_rule(&mut self, rule: KaraokeRunRule) -> usize {
        self.rules.push(rule);
        self.rules.len() - 1
    }

    /// Записывает роль очередной выданной картинки.
    pub(crate) fn push_image(&mut self, role: ImageRole) {
        self.roles.push(role);
    }

    /// Готовая стадия; `None`, если в реплике нет ни одного правила (караоке не нужно).
    pub(crate) fn finish(self) -> Option<KaraokeStage> {
        (!self.rules.is_empty()).then_some(KaraokeStage {
            rules: self.rules,
            roles: self.roles,
        })
    }
}
