//! Состояние последнего кадра: подпись запроса и результат.
//!
//! Подпись нужна для быстрого пути «ничего не изменилось»: сравнение с новым запросом идёт
//! по месту, без аллокаций и без обхода содержимого реплик (доверие ключам `CueKey`).

use super::request::{CueKey, RenderRequest};
use crate::appearance::SubtitleAppearance;
use crate::overlay::{CanvasSize, SubtitleOverlay};

/// Подпись запроса: ключи реплик по порядку, холст и стиль.
#[derive(Debug, Clone)]
pub(crate) struct FrameSignature {
    keys: Vec<CueKey>,
    canvas: CanvasSize,
    appearance: SubtitleAppearance,
}

impl FrameSignature {
    /// Снимает подпись запроса.
    pub(crate) fn of(request: &RenderRequest<'_>) -> Self {
        Self {
            keys: request.cues.iter().map(|cue| cue.key).collect(),
            canvas: request.canvas,
            appearance: *request.appearance,
        }
    }

    /// Совпадает ли запрос с подписью (без аллокаций).
    pub(crate) fn matches(&self, request: &RenderRequest<'_>) -> bool {
        self.canvas == request.canvas
            && self.appearance == *request.appearance
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
