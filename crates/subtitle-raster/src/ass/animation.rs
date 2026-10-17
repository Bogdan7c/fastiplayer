// Порт из libass 0.17.5: ass_parse.c (теги `\move`, `\fad`, `\fade` в `ass_parse_tags`,
// `interpolate_alpha`).
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

//! Анимация реплики ASS: что в ней зависит от времени показа и как это выглядит СЕЙЧАС.
//!
//! Зависят от времени ровно два тега: `\move` (положение) и `\fad`/`\fade` (затухание).
//! Остальное (`\t` и пр.) пока не поддержано. Модуль владеет всем, что связано со временем:
//! - [`AssEventClock`] — «сколько мс прошло от начала реплики» (libass: `render_priv->time -
//!   event->Start`) и длительность реплики;
//! - [`evaluate_animation`] — по тексту реплики и часам даёт [`AssAnimation`]: текущее
//!   положение (если оно движется), текущее затухание, ключ кэша и ближайшее изменение.
//!
//! ## Почему отдельно от `RenderState`
//! Ключ кэша реплики должен быть известен ДО разбора текста в символы и шрифты. Поэтому
//! модуль сам быстро просматривает теги реплики (без шрифтов, без выделений), применяя те же
//! правила «побеждает первый»: первый позиционирующий тег из `\pos`/`\move` (оба ставят
//! `EVENT_POSITIONED`) и первый тег затухания (`PARSED_FADE`).
//!
//! ## Арифметика как в libass
//! Времена и альфы — `int32`; вычитание времён идёт через `uint32` (оборачивание без паники),
//! прогресс `\move` — `double`, альфа затухания усекается к целому.

use subtitle_core::{AssCue, AssFade, AssMove, AssOverride, AssTextItem, SubtitleTime};

use super::render_state::ScriptPoint;
use crate::event_fade::EventFade;
use crate::overlay::AnimationChange;
use crate::renderer::AssEventTiming;

/// Микросекунд в миллисекунде: шкала `SubtitleTime` против шкалы ASS.
const MICROS_PER_MILLI: i64 = 1_000;

/// Часы реплики: сколько прошло от её начала и сколько она длится (мс, как в libass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AssEventClock {
    /// Начало реплики (для пересчёта «мс от начала» обратно в момент).
    start: SubtitleTime,
    /// `render_priv->time - event->Start`; отрицательно до начала реплики.
    elapsed_ms: i32,
    /// `event->Duration`.
    duration_ms: i32,
}

impl AssEventClock {
    /// Часы реплики с таймингом `timing` в момент показа `moment`.
    pub(crate) fn new(moment: SubtitleTime, timing: AssEventTiming) -> Self {
        let elapsed_micros = moment
            .as_micros()
            .saturating_sub(timing.start().as_micros());
        let duration_micros = timing
            .end()
            .as_micros()
            .saturating_sub(timing.start().as_micros());
        Self {
            start: timing.start(),
            elapsed_ms: saturating_millis(elapsed_micros),
            duration_ms: saturating_millis(duration_micros).max(0),
        }
    }

    /// Момент, отстоящий от начала реплики на `offset_ms` (мс могут быть и отрицательными).
    fn moment_at(&self, offset_ms: i64) -> SubtitleTime {
        SubtitleTime::from_micros(
            self.start
                .as_micros()
                .saturating_add(offset_ms.saturating_mul(MICROS_PER_MILLI)),
        )
    }
}

/// Микросекунды -> миллисекунды вниз (к минус бесконечности: libass живёт в целых мс) с
/// насыщением в `i32`.
fn saturating_millis(micros: i64) -> i32 {
    let millis = micros.div_euclid(MICROS_PER_MILLI);
    i32::try_from(millis).unwrap_or(if millis < 0 { i32::MIN } else { i32::MAX })
}

/// Мгновенное состояние анимации: то, что зависит от момента показа.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AssAnimationState {
    /// Положение реплики от `\move` в координатах сценария; `None` — реплика не движется
    /// (нет `\move`, первым позиционирующим тегом стоит `\pos`, или начало и конец совпадают):
    /// тогда положение целиком определяет `RenderState`.
    pub(crate) position: Option<ScriptPoint>,
    /// Затухание на этот момент.
    pub(crate) fade: EventFade,
}

impl AssAnimationState {
    /// Без анимации.
    pub(crate) const STILL: Self = Self {
        position: None,
        fade: EventFade::NONE,
    };
}

/// Ключ кэша для времязависимого состояния реплики.
///
/// Положение хранится точными битами `f64`: рисование зависит от дробной части положения
/// (позиция квантуется конвейером до 1/64 пикселя уже после перевода в пиксели и сложения с
/// опорной точкой блока), поэтому честная и безопасная единица сравнения — само число.
/// Грубее квантовать нельзя без отхода от libass; выигрыша бы и не было: на движущейся
/// реплике соседние кадры всё равно различны, а один и тот же момент даёт то же число.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AssAnimationKey {
    /// Реплика от времени не зависит: ключ без времени (статичная сцена не рисуется заново).
    TimeFree,
    /// Реплика анимирована: состояние на момент показа.
    Sampled {
        /// Биты `x`, `y` положения от `\move`.
        position_bits: Option<[u64; 2]>,
        /// Затухание (число libass).
        fade: i32,
    },
}

/// Состояние анимации реплики в момент показа и сведения о будущих изменениях.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AssAnimation {
    /// Состояние на момент показа.
    pub(crate) state: AssAnimationState,
    /// Ближайшее изменение картинки реплики.
    pub(crate) change: AnimationChange,
    /// В реплике нет ни движущегося `\move`, ни затухания: результат не зависит от момента.
    time_free: bool,
}

impl AssAnimation {
    /// Реплика без анимации.
    const TIME_FREE: Self = Self {
        state: AssAnimationState::STILL,
        change: AnimationChange::None,
        time_free: true,
    };

    /// Результат не зависит от момента показа вообще (в том числе при перемотке назад).
    pub(crate) fn is_time_free(&self) -> bool {
        self.time_free
    }

    /// Ключ кэша реплики по времязависимому состоянию.
    pub(crate) fn cache_key(&self) -> AssAnimationKey {
        if self.time_free {
            return AssAnimationKey::TimeFree;
        }
        AssAnimationKey::Sampled {
            position_bits: self
                .state
                .position
                .map(|point| [point.x.to_bits(), point.y.to_bits()]),
            fade: self.state.fade.value(),
        }
    }
}

/// `\move`, приведённый к границам времени (libass: хвост ветки `move` в `ass_parse_tags`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct ResolvedMove {
    from: ScriptPoint,
    to: ScriptPoint,
    /// Начало движения, мс от начала реплики.
    start_ms: i32,
    /// Конец движения, мс от начала реплики.
    end_ms: i32,
}

impl ResolvedMove {
    /// Применяет правила libass: `t1 > t2` -> обмен; `t1 <= 0 && t2 <= 0` -> на всю реплику.
    fn new(movement: &AssMove, duration_ms: i32) -> Self {
        let (mut start_ms, mut end_ms) = (movement.start_ms, movement.end_ms);
        if start_ms > end_ms {
            std::mem::swap(&mut start_ms, &mut end_ms);
        }
        if start_ms <= 0 && end_ms <= 0 {
            (start_ms, end_ms) = (0, duration_ms);
        }
        Self {
            from: ScriptPoint {
                x: movement.from_x,
                y: movement.from_y,
            },
            to: ScriptPoint {
                x: movement.to_x,
                y: movement.to_y,
            },
            start_ms,
            end_ms,
        }
    }

    /// Начало и конец совпадают: реплика не движется.
    fn is_motionless(&self) -> bool {
        self.from == self.to
    }

    /// Положение на `elapsed_ms` (линейно; ускорения нет).
    fn position_at(&self, elapsed_ms: i32) -> ScriptPoint {
        let progress = if elapsed_ms <= self.start_ms {
            0.0
        } else if elapsed_ms >= self.end_ms {
            1.0
        } else {
            // libass: (int32)((uint32)t - t1) / (int32)((uint32)t2 - t1).
            let passed = wrapping_difference(elapsed_ms, self.start_ms);
            let span = wrapping_difference(self.end_ms, self.start_ms);
            f64::from(passed) / f64::from(span)
        };
        ScriptPoint {
            x: progress * (self.to.x - self.from.x) + self.from.x,
            y: progress * (self.to.y - self.from.y) + self.from.y,
        }
    }

    /// Когда положение изменится относительно `elapsed_ms`.
    fn change_after(&self, clock: &AssEventClock) -> AnimationChange {
        if self.is_motionless() || clock.elapsed_ms >= self.end_ms {
            AnimationChange::None
        } else if clock.elapsed_ms <= self.start_ms {
            // На `t == t1` положение ещё начальное, первая смена — на следующей миллисекунде.
            AnimationChange::At(clock.moment_at(i64::from(self.start_ms) + 1))
        } else {
            AnimationChange::EveryFrame
        }
    }
}

/// Разность времён по правилам `uint32` libass (оборачивание вместо паники).
fn wrapping_difference(later: i32, earlier: i32) -> i32 {
    (later as u32).wrapping_sub(earlier as u32) as i32
}

/// Кривая затухания `\fad`/`\fade` с пересчитанными временами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FadeCurve {
    /// `a1, a2, a3`: прозрачность до `t1`, между `t2` и `t3`, после `t4`.
    alphas: [i32; 3],
    /// `t1..t4`, мс от начала реплики.
    times_ms: [i32; 4],
}

impl FadeCurve {
    /// Строит кривую из сырого тега (libass: ветка `fade`/`fad`).
    fn new(fade: &AssFade, duration_ms: i32) -> Self {
        let (alphas, mut times_ms) = match *fade {
            AssFade::Simple {
                fade_in_ms,
                fade_out_ms,
            } => ([0xFF, 0, 0xFF], [-1, fade_in_ms, fade_out_ms, -1]),
            AssFade::Full { alphas, times_ms } => (alphas, times_ms),
        };
        if times_ms[0] == -1 && times_ms[3] == -1 {
            // Длительность реплики: появление с начала, угасание к концу.
            times_ms[0] = 0;
            times_ms[3] = duration_ms;
            times_ms[2] = wrapping_difference(duration_ms, times_ms[2]);
        }
        Self { alphas, times_ms }
    }

    /// Прозрачность на `elapsed_ms` (libass: `interpolate_alpha`).
    fn alpha_at(&self, elapsed_ms: i32) -> i32 {
        let [a1, a2, a3] = self.alphas;
        let [t1, t2, t3, t4] = self.times_ms;
        if elapsed_ms < t1 {
            a1
        } else if elapsed_ms < t2 {
            blend(a1, a2, ramp_progress(elapsed_ms, t1, t2))
        } else if elapsed_ms < t3 {
            a2
        } else if elapsed_ms < t4 {
            blend(a2, a3, ramp_progress(elapsed_ms, t3, t4))
        } else {
            a3
        }
    }

    /// Когда затухание изменится относительно `elapsed_ms`. Ветки повторяют `alpha_at`.
    fn change_after(&self, clock: &AssEventClock) -> AnimationChange {
        let [a1, a2, a3] = self.alphas;
        let [t1, t2, t3, t4] = self.times_ms;
        let elapsed = clock.elapsed_ms;
        let at = |offset_ms: i32| AnimationChange::At(clock.moment_at(i64::from(offset_ms)));
        if elapsed < t1 {
            at(t1)
        } else if elapsed < t2 {
            if a1 == a2 {
                at(t2)
            } else {
                AnimationChange::EveryFrame
            }
        } else if elapsed < t3 {
            at(t3)
        } else if elapsed < t4 {
            if a2 == a3 {
                at(t4)
            } else {
                AnimationChange::EveryFrame
            }
        } else {
            AnimationChange::None
        }
    }
}

/// Доля пройденного участка `[from, to)` (libass: `cf`).
fn ramp_progress(elapsed_ms: i32, from_ms: i32, to_ms: i32) -> f64 {
    f64::from(wrapping_difference(elapsed_ms, from_ms))
        / f64::from(wrapping_difference(to_ms, from_ms))
}

/// Линейная смесь альф с усечением к целому (libass: присваивание `double` в `int`).
fn blend(from: i32, to: i32, progress: f64) -> i32 {
    (f64::from(from) * (1.0 - progress) + f64::from(to) * progress) as i32
}

/// Теги времени реплики с учётом правила «побеждает первый».
struct TimeTags<'cue> {
    /// `\move`, если первым позиционирующим тегом реплики стоит именно он.
    movement: Option<&'cue AssMove>,
    /// Первый `\fad`/`\fade`.
    fade: Option<&'cue AssFade>,
}

/// Быстрый просмотр тегов реплики (без разбора текста на символы).
fn scan_time_tags(cue: &AssCue) -> TimeTags<'_> {
    let mut movement = None;
    let mut positioned = false;
    let mut fade = None;
    for item in &cue.text {
        let AssTextItem::Overrides(overrides) = item else {
            continue;
        };
        for tag in overrides {
            match tag {
                // `\pos` и `\move` оба ставят `EVENT_POSITIONED`: второй из них игнорируется.
                AssOverride::Position { .. } => positioned = true,
                AssOverride::Move(candidate) if !positioned => {
                    movement = Some(candidate);
                    positioned = true;
                }
                AssOverride::Fade(candidate) if fade.is_none() => fade = Some(candidate),
                _ => {}
            }
        }
    }
    TimeTags { movement, fade }
}

/// Состояние анимации реплики `cue` на часах `clock`.
pub(crate) fn evaluate_animation(cue: &AssCue, clock: &AssEventClock) -> AssAnimation {
    let tags = scan_time_tags(cue);
    let movement = tags
        .movement
        .map(|raw| ResolvedMove::new(raw, clock.duration_ms))
        .filter(|resolved| !resolved.is_motionless());
    let fade = tags.fade.map(|raw| FadeCurve::new(raw, clock.duration_ms));
    if movement.is_none() && fade.is_none() {
        return AssAnimation::TIME_FREE;
    }
    let position = movement.map(|resolved| resolved.position_at(clock.elapsed_ms));
    let fade_value = fade.map_or(0, |curve| curve.alpha_at(clock.elapsed_ms));
    let change = movement
        .map_or(AnimationChange::None, |resolved| {
            resolved.change_after(clock)
        })
        .earliest(fade.map_or(AnimationChange::None, |curve| curve.change_after(clock)));
    AssAnimation {
        state: AssAnimationState {
            position,
            fade: EventFade::new(fade_value),
        },
        change,
        time_free: false,
    }
}

#[cfg(test)]
mod tests;
