//! Состояние последнего кадра: подпись запроса и результат.
//!
//! Подпись нужна для быстрого пути «ничего не изменилось»: сравнение с новым запросом идёт
//! по месту, без аллокаций и без обхода содержимого реплик (доверие ключам `CueKey`).

use super::request::{CueContent, CueKey, RenderRequest};
use crate::appearance::SubtitleAppearance;
use crate::ass::AssRenderOptions;
use crate::fonts::TextLanguagePreference;
use crate::overlay::{CanvasSize, SubtitleOverlay};

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
            .any(|cue| matches!(cue.content, CueContent::Ass(_)))
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

/// Последний выданный кадр.
#[derive(Debug)]
pub(crate) struct FrameState {
    /// Подпись запроса, из которого получен кадр.
    pub(crate) signature: FrameSignature,
    /// Выданный результат.
    pub(crate) overlay: SubtitleOverlay,
    /// Кадр устарел (например, поменялись шрифты) и не может быть выдан повторно «как есть».
    pub(crate) stale: bool,
}

impl FrameState {
    /// Байты масок картинок кадра.
    pub(crate) fn mask_bytes(&self) -> usize {
        self.overlay
            .bitmaps()
            .iter()
            .fold(0_usize, |total, bitmap| {
                total.saturating_add(bitmap.coverage().len())
            })
    }

    /// Можно ли ответить на запрос прежним кадром.
    pub(crate) fn can_serve(&self, request: &RenderRequest<'_>) -> bool {
        !self.stale && self.signature.matches(request)
    }
}
