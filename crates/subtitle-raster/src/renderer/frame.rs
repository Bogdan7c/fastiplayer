//! Состояние последнего кадра: подпись запроса и результат.
//!
//! Подпись нужна для быстрого пути «ничего не изменилось»: сравнение с новым запросом идёт
//! по месту, без аллокаций и без обхода содержимого реплик (доверие ключам `CueKey`).

use subtitle_core::SubtitleTime;

use super::request::{CueContent, CueKey, RenderRequest};
use crate::appearance::SubtitleAppearance;
use crate::ass::{AssAnimation, AssRenderOptions};
use crate::fonts::TextLanguagePreference;
use crate::overlay::{AnimationChange, CanvasSize, SubtitleOverlay};

/// Подпись запроса: ключи реплик по порядку, холст, стиль и язык текста.
#[derive(Debug, Clone)]
pub(crate) struct FrameSignature {
    keys: Vec<CueKey>,
    canvas: CanvasSize,
    appearance: SubtitleAppearance,
    text_language: TextLanguagePreference,
    /// Настройки ASS; `None`, если в кадре нет ASS-реплик (тогда их смена кадр не меняет).
    ass: Option<AssRenderOptions>,
}

impl FrameSignature {
    /// Снимает подпись запроса.
    pub(crate) fn of(request: &RenderRequest<'_>) -> Self {
        Self {
            keys: request.cues.iter().map(|cue| cue.key).collect(),
            canvas: request.canvas,
            appearance: *request.appearance,
            text_language: request.text_language,
            ass: Self::ass_options_of(request),
        }
    }

    /// Настройки ASS, если они влияют на кадр.
    fn ass_options_of(request: &RenderRequest<'_>) -> Option<AssRenderOptions> {
        request
            .cues
            .iter()
            .any(|cue| matches!(cue.content, CueContent::Ass { .. }))
            .then_some(request.ass)
    }

    /// Совпадает ли запрос с подписью (без аллокаций).
    pub(crate) fn matches(&self, request: &RenderRequest<'_>) -> bool {
        self.canvas == request.canvas
            && self.appearance == *request.appearance
            && self.text_language == request.text_language
            && self.ass == Self::ass_options_of(request)
            && self.keys.len() == request.cues.len()
            && self
                .keys
                .iter()
                .zip(request.cues)
                .all(|(known, cue)| *known == cue.key)
    }
}

/// Зависимость собираемого кадра от времени: сводка по его ASS-репликам.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameTimeDependence {
    /// Ближайшее изменение анимации среди реплик кадра.
    pub(crate) change: AnimationChange,
    /// Ни одна реплика кадра от момента показа не зависит (даже при перемотке назад).
    pub(crate) time_free: bool,
}

impl FrameTimeDependence {
    /// Кадр без ASS-анимаций (отправная точка сводки).
    pub(crate) const FREE: Self = Self {
        change: AnimationChange::None,
        time_free: true,
    };

    /// Учитывает анимацию очередной ASS-реплики кадра.
    pub(crate) fn include(&mut self, animation: &AssAnimation) {
        self.change = self.change.earliest(animation.change);
        self.time_free &= animation.is_time_free();
    }
}

impl FrameTimeDependence {
    /// Учитывает реплику со словами, появляющимися по времени (`TextSpan::reveal_at`):
    /// `next_reveal` — ближайшее появление после момента кадра (`None` — всё уже видно).
    /// Кадр перестаёт быть «вне времени» в любом случае: при перемотке назад слова скрываются.
    pub(crate) fn include_reveal(&mut self, next_reveal: Option<SubtitleTime>) {
        if let Some(reveal_at) = next_reveal {
            self.change = self.change.earliest(AnimationChange::At(reveal_at));
        }
        self.time_free = false;
    }

    /// Учитывает изменение, которое реплике добавила стадия после кэша (караоке): оно известно
    /// только по готовой реплике, поэтому не входит в `AssAnimation`.
    pub(crate) fn include_stage_change(&mut self, change: AnimationChange) {
        self.change = self.change.earliest(change);
        self.time_free = false;
    }
}

/// Последний выданный кадр.
#[derive(Debug)]
pub(crate) struct FrameState {
    /// Подпись запроса, из которого получен кадр.
    pub(crate) signature: FrameSignature,
    /// Выданный результат.
    pub(crate) overlay: SubtitleOverlay,
    /// Кадр устарел (например, поменялись шрифты) и не может быть выдан повторно «как есть».
    pub(crate) stale: bool,
    /// Момент показа, для которого кадр построен.
    pub(crate) rendered_at: SubtitleTime,
    /// Как кадр зависит от времени (для быстрого пути).
    pub(crate) time: FrameTimeDependence,
}

impl FrameState {
    /// Байты масок картинок кадра.
    pub(crate) fn mask_bytes(&self) -> usize {
        self.overlay
            .bitmaps()
            .iter()
            .fold(0_usize, |total, bitmap| {
                total.saturating_add(bitmap.pixel_bytes())
            })
    }

    /// Можно ли ответить на запрос прежним кадром.
    ///
    /// Кроме совпадения подписи нужно, чтобы время не успело изменить картинку: статичная
    /// сцена годится на любой момент (нулевая работа), анимированная — пока
    /// [`AnimationChange::holds_at`] подтверждает прежний кадр.
    pub(crate) fn can_serve(&self, request: &RenderRequest<'_>) -> bool {
        !self.stale && self.signature.matches(request) && self.holds_at(request.moment)
    }

    /// Прежний кадр верен для момента `moment`.
    fn holds_at(&self, moment: SubtitleTime) -> bool {
        self.time.time_free
            || self
                .overlay
                .animation_change()
                .holds_at(self.rendered_at, moment)
    }
}
