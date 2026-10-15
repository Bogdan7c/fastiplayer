//! Входные типы растеризатора: что рисовать на кадре.

use subtitle_core::{SubtitleEventId, SubtitleTextCue};

use crate::appearance::SubtitleAppearance;
use crate::overlay::CanvasSize;

/// Источник реплик: дорожка и её «поколение» (идентификатор задаёт вызывающий).
///
/// Меняйте его, когда содержимое дорожки могло измениться (загружена другая дорожка,
/// дорожка перечитана после перемотки с дублями): тогда кэш не спутает старые и новые
/// реплики с одинаковыми `SubtitleEventId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CueSourceId(u64);

impl CueSourceId {
    /// Оборачивает идентификатор источника.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Численное значение.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Идентичность реплики для кэша: источник + идентификатор события в нём.
///
/// КОНТРАКТ: одинаковый ключ означает ОДИНАКОВОЕ содержимое реплики (текст, стили,
/// размещение). Растеризатор доверяет ключу и не сравнивает содержимое: так проверка «ничего
/// не изменилось» не требует ни аллокаций, ни обхода текста. Изменилось содержимое —
/// выдайте новый [`CueSourceId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CueKey {
    source: CueSourceId,
    event: SubtitleEventId,
}

impl CueKey {
    /// Ключ реплики `event` из источника `source`.
    pub const fn new(source: CueSourceId, event: SubtitleEventId) -> Self {
        Self { source, event }
    }

    /// Источник реплики.
    pub const fn source(&self) -> CueSourceId {
        self.source
    }

    /// Идентификатор события в источнике.
    pub const fn event(&self) -> SubtitleEventId {
        self.event
    }
}

/// Реплика кадра: ключ и ссылка на содержимое.
#[derive(Debug, Clone, Copy)]
pub struct RenderableCue<'a> {
    /// Идентичность реплики.
    pub key: CueKey,
    /// Содержимое (читается только при промахе кэша).
    pub cue: &'a SubtitleTextCue,
}

/// Запрос на один кадр.
#[derive(Debug, Clone, Copy)]
pub struct RenderRequest<'a> {
    /// Активные реплики в порядке отрисовки (первая — самая нижняя; при столкновениях
    /// первая остаётся на месте, следующие сдвигаются).
    pub cues: &'a [RenderableCue<'a>],
    /// Холст — размер картинки видео на экране.
    pub canvas: CanvasSize,
    /// Стиль пользователя.
    pub appearance: &'a SubtitleAppearance,
}

/// Лимиты кэшей растеризатора. Значений по умолчанию нет: их задаёт конфиг (сессия 04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterCacheLimits {
    /// Сколько байт масок готовых реплик держать в кэше.
    pub cue_bitmaps_bytes: usize,
    /// Сколько подготовленных лиц шрифта (таблицы шейпера, метрики) держать в кэше.
    ///
    /// Кэш глифовых битмапов отсутствует (см. `cache`), отдельного лимита на него нет.
    pub shaper_faces: usize,
}
