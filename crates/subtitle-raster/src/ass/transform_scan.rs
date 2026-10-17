//! Быстрый просмотр `\t` реплики: что в них анимируется и как это влияет на кэш и срок годности.
//!
//! Просмотр идёт по тексту реплики ДО разбора на символы и шрифты (как `scan_time_tags` для
//! `\move`/`\fad`), поэтому здесь нет ничего про шрифты: только вопросы к списку тегов.
//!
//! ## Три вида эффекта вложенного тега ([`TransformEffect`])
//! - [`TransformEffect::Raster`]: меняет вид букв (размер, цвет, обводка, поворот, размытие...) —
//!   вес `\t` входит в ключ кэша реплики, реплика рисуется заново при каждом новом весе;
//! - [`TransformEffect::RectClip`]: прямоугольный клип — стадия ПОСЛЕ кэша, в ключ не входит
//!   (решение владельца): клип вычисляется на момент кадра (`ass::clip_animation`);
//! - [`TransformEffect::None`]: тег применяется сразу и целиком (`\pos`, `\b`, `\fn`...) либо
//!   не имеет аргумента — вес на него не влияет, а значит, и во времени он не меняется.

use subtitle_core::{AssClipShape, AssCue, AssFontSize, AssOverride, AssTextItem, AssTransform};

use super::animation::AssEventClock;
use super::tag_weight::TransformSpan;
use crate::overlay::AnimationChange;

/// Чем вложенный тег зависит от веса `\t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransformEffect {
    /// Вес не влияет (тег без анимации или без аргумента).
    None,
    /// Вес меняет растеризацию реплики.
    Raster,
    /// Вес меняет прямоугольный клип (стадия после кэша).
    RectClip,
}

/// Эффект тега: «анимируемые» теги из `ass_parse_tags`, у которых есть аргумент. Тег без
/// аргумента (`\fs`, `\bord`, `\c`...) возвращает значение стиля сразу, не используя `pwr`.
pub(crate) fn effect_of(tag: &AssOverride) -> TransformEffect {
    let animated = match tag {
        AssOverride::Border { value, .. } | AssOverride::Shadow { value, .. } => value.is_some(),
        AssOverride::Shear { factor, .. } => factor.is_some(),
        AssOverride::Rotation { degrees, .. } => degrees.is_some(),
        AssOverride::ScaleX(value) | AssOverride::ScaleY(value) | AssOverride::Spacing(value) => {
            value.is_some()
        }
        AssOverride::FontSize(size) => !matches!(size, AssFontSize::Reset),
        AssOverride::Color { value, .. } => value.is_some(),
        AssOverride::Alpha { value, .. } => value.is_some(),
        AssOverride::Blur(value) | AssOverride::EdgeBlur(value) => value.is_some(),
        AssOverride::Clip(clip) => {
            return if matches!(clip.shape, AssClipShape::Rect { .. }) {
                TransformEffect::RectClip
            } else {
                TransformEffect::None
            };
        }
        _ => false,
    };
    if animated {
        TransformEffect::Raster
    } else {
        TransformEffect::None
    }
}

/// Тег меняет поворот или наклон (`\frx \fry \frz \fr \fax \fay`) с аргументом.
fn is_orientation_tag(tag: &AssOverride) -> bool {
    matches!(
        tag,
        AssOverride::Rotation {
            degrees: Some(_),
            ..
        } | AssOverride::Shear {
            factor: Some(_),
            ..
        }
    )
}

/// Обходит все `\t` реплики в порядке записи, включая вложенные (сначала внешний).
fn for_each_transform<'cue>(cue: &'cue AssCue, visit: &mut impl FnMut(&'cue AssTransform)) {
    for item in &cue.text {
        if let AssTextItem::Overrides(overrides) = item {
            visit_in_list(overrides, visit);
        }
    }
}

fn visit_in_list<'cue>(overrides: &'cue [AssOverride], visit: &mut impl FnMut(&'cue AssTransform)) {
    for tag in overrides {
        if let AssOverride::Transform(transform) = tag {
            visit(transform);
            visit_in_list(&transform.tags, visit);
        }
    }
}

/// Сводка по `\t` реплики на момент часов.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct TransformDigest {
    /// Биты весов `\t`, которые меняют растеризацию (в порядке записи): часть ключа кэша.
    pub(crate) raster_weights: Vec<u64>,
    /// Ближайшее изменение картинки из-за любого `\t`.
    pub(crate) change: Option<AnimationChange>,
    /// Хоть один `\t` меняет картинку во времени (включая клип-только `\t`).
    pub(crate) time_dependent: bool,
}

impl TransformDigest {
    /// Ближайшее изменение (`None`-вариант перечисления, если анимаций нет).
    pub(crate) fn change(&self) -> AnimationChange {
        self.change.unwrap_or(AnimationChange::None)
    }
}

/// Сводка по `\t` реплики `cue` на часах `clock`.
pub(crate) fn digest_transforms(cue: &AssCue, clock: &AssEventClock) -> TransformDigest {
    let mut digest = TransformDigest::default();
    for_each_transform(cue, &mut |transform| {
        let mut animates_raster = false;
        let mut animates_anything = false;
        for tag in &transform.tags {
            match effect_of(tag) {
                TransformEffect::Raster => {
                    animates_raster = true;
                    animates_anything = true;
                }
                TransformEffect::RectClip => animates_anything = true,
                TransformEffect::None => {}
            }
        }
        if !animates_anything {
            return;
        }
        let span = TransformSpan::new(&transform.timing, clock.duration_ms());
        if animates_raster {
            digest
                .raster_weights
                .push(span.weight_at(clock.elapsed_ms()).to_bits());
        }
        digest.time_dependent = true;
        let change = change_after(&span, clock);
        digest.change = Some(digest.change.map_or(change, |known| known.earliest(change)));
    });
    digest
}

/// Когда вес `\t` изменится относительно текущего момента часов.
///
/// - До начала вес 0 (а на самом `t1` — тоже, если `pow(0, accel) == 0`): картинка постоянна
///   до следующей миллисекунды после `t1`, как у `\move`; если вес на `t1` уже не 0 (`accel`
///   не положительно, или `t2 <= t1` и вес сразу 1) — до самого `t1`.
/// - Внутри `[t1, t2)` — каждый кадр.
/// - После `t2` вес 1 навсегда.
fn change_after(span: &TransformSpan, clock: &AssEventClock) -> AnimationChange {
    let elapsed = clock.elapsed_ms();
    let weight_at_start_is_zero = span.weight_at(span.start_ms).value() == 0.0;
    if elapsed < span.start_ms || (elapsed == span.start_ms && weight_at_start_is_zero) {
        let first_change_ms = i64::from(span.start_ms) + i64::from(weight_at_start_is_zero);
        return AnimationChange::At(clock.moment_at(first_change_ms));
    }
    if elapsed < span.end_ms {
        AnimationChange::EveryFrame
    } else {
        AnimationChange::None
    }
}

/// В реплике есть `\t` с анимацией прямоугольного клипа: клип считается покадрово.
pub(crate) fn animates_rect_clip(cue: &AssCue) -> bool {
    let mut found = false;
    for_each_transform(cue, &mut |transform| {
        found |= transform
            .tags
            .iter()
            .any(|tag| effect_of(tag) == TransformEffect::RectClip);
    });
    found
}

/// В реплике есть `\t`, который поворачивает или наклоняет текст: такая реплика — «надпись»
/// (группа размещения `Fixed`) независимо от того, виден ли поворот в данный момент.
pub(crate) fn animates_orientation(cue: &AssCue) -> bool {
    let mut found = false;
    for_each_transform(cue, &mut |transform| {
        found |= transform.tags.iter().any(is_orientation_tag);
    });
    found
}

#[cfg(test)]
mod tests;
