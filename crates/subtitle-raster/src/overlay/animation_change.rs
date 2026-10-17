//! Когда картинка субтитров изменится сама по себе, без смены набора реплик.
//!
//! Анимации ASS (`\move`, `\fad`) делают кадр функцией времени. Растеризатор не владеет
//! часами, зато знает, до какого момента нарисованное остаётся верным: об этом сообщает
//! [`AnimationChange`]. Вызывающий (рисовальщик в фоне) по нему решает, на какой срок кадр
//! действителен и когда нужен следующий.

use subtitle_core::SubtitleTime;

/// Ближайшее изменение анимации среди нарисованных реплик (для момента запроса).
///
/// Порядок «срочности»: [`Self::EveryFrame`] раньше любого [`Self::At`], а [`Self::None`]
/// позже всего. Значение относится к МОМЕНТАМ НЕ РАНЬШЕ момента запроса: о более ранних
/// моментах (перемотка назад) оно ничего не обещает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationChange {
    /// Картинка не меняется со временем (статичная сцена или анимации уже завершились).
    None,
    /// Картинка меняется на каждом кадре, начиная с этого момента (идёт `\move` или затухание).
    EveryFrame,
    /// Картинка остаётся прежней строго до этого момента, потом может измениться (например,
    /// до начала `\move` или между фазами `\fad`).
    At(SubtitleTime),
}

impl AnimationChange {
    /// Более раннее из двух изменений (минимум по репликам кадра).
    pub fn earliest(self, other: Self) -> Self {
        match (self, other) {
            (Self::None, rest) | (rest, Self::None) => rest,
            (Self::EveryFrame, _) | (_, Self::EveryFrame) => Self::EveryFrame,
            (Self::At(first), Self::At(second)) => Self::At(first.min(second)),
        }
    }

    /// Картинка, нарисованная для момента `rendered_at`, верна и для `moment` без перерисовки.
    ///
    /// Ответ `true` только для моментов не раньше `rendered_at` (см. описание типа).
    pub fn holds_at(self, rendered_at: SubtitleTime, moment: SubtitleTime) -> bool {
        if moment < rendered_at {
            return false;
        }
        match self {
            Self::None => true,
            Self::EveryFrame => moment == rendered_at,
            Self::At(change_at) => moment < change_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: i64) -> AnimationChange {
        AnimationChange::At(SubtitleTime::from_millis(millis))
    }

    #[test]
    fn earliest_orders_every_frame_before_at_before_none() {
        assert_eq!(
            AnimationChange::None.earliest(AnimationChange::None),
            AnimationChange::None
        );
        assert_eq!(AnimationChange::None.earliest(at(5)), at(5));
        assert_eq!(at(5).earliest(at(3)), at(3));
        assert_eq!(at(3).earliest(at(5)), at(3));
        assert_eq!(
            at(3).earliest(AnimationChange::EveryFrame),
            AnimationChange::EveryFrame
        );
        assert_eq!(
            AnimationChange::EveryFrame.earliest(AnimationChange::None),
            AnimationChange::EveryFrame
        );
    }

    #[test]
    fn holds_at_respects_every_variant_and_never_looks_back() {
        let rendered = SubtitleTime::from_millis(100);
        let later = SubtitleTime::from_millis(150);
        let earlier = SubtitleTime::from_millis(50);
        assert!(AnimationChange::None.holds_at(rendered, later));
        assert!(!AnimationChange::None.holds_at(rendered, earlier));
        assert!(AnimationChange::EveryFrame.holds_at(rendered, rendered));
        assert!(!AnimationChange::EveryFrame.holds_at(rendered, later));
        assert!(at(200).holds_at(rendered, later));
        assert!(!at(150).holds_at(rendered, later));
        assert!(!at(200).holds_at(rendered, earlier));
    }
}
