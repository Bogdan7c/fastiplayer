//! Караоке `\k`, `\K`, `\kf`, `\ko`, `\kt`: стадия «после кэша реплик».
//!
//! ## Решение владельца
//! Буквы караоке-строки растеризуются ОДИН раз; каждый кадр меняются только граница заливки,
//! выбор цвета и видимость обводки `\ko`. Поэтому время в ключ кэша реплики не входит, а всё,
//! что зависит от момента кадра, живёт здесь и в стадии [`crate::clip`] (разрез картинки по
//! `brk` выполняется там же, где клип: так порядок картинок совпадает с `render_glyph` libass).
//!
//! ## Кто чем владеет
//! - [`KaraokeMark`] — сырой захват тегов на символ (libass: `effect_type`, `effect_timing`,
//!   `effect_skip_timing`, `reset_effect` в `GlyphInfo`); ставит его разбор реплики
//!   (`ass::RenderState`), режет по нему участки `shaping::mark_style_runs`.
//! - [`KaraokePlan`] — «слова» караоке одной раскладки и их время, не зависящее от кадра
//!   (`ass_process_karaoke_effects` без `tm_current`); строится в `layout::arrange_text` между
//!   переносом строк и порядком BiDi, потому что читает предварительные позиции глифов.
//! - [`KaraokeRunRule`] — описание одного участка в готовой реплике (слово, самая левая точка
//!   контура на экране, цвета) и функция «что рисовать на момент `t`».
//! - [`KaraokeStage`] — правила реплики и роль каждой её картинки; отдаёт покадровые
//!   `ImagePaint` и срок годности картинки (`AnimationChange`).
//!
//! ## Арифметика libass
//! Времена — `int32`/`long long`, позиции — 26.6, `dt` — `double`, округления — `ass_lrint`
//! (к чётному) для границы слова и `lround` (от нуля) для экранной границы. Повторено как есть.

mod leftmost;
mod mark;
mod plan;
mod rule;
mod stage;

pub(crate) use leftmost::LeftmostTracker;
pub(crate) use mark::{KaraokeEffect, KaraokeMark};
pub(crate) use plan::{KaraokePlan, KaraokeWord};
pub(crate) use rule::{KaraokeRunRule, NextChange, RuleInputs};
pub(crate) use stage::{ImageRole, KaraokeStage, KaraokeStageBuilder};

#[cfg(test)]
mod tests;
