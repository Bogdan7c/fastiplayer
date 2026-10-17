//! Входные типы растеризатора: что рисовать на кадре.

use subtitle_core::{AssCue, SubtitleEventId, SubtitleLayer, SubtitleTextCue};

use crate::appearance::SubtitleAppearance;
use crate::ass::AssRenderOptions;
use crate::fonts::TextLanguagePreference;
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

/// Содержимое реплики кадра.
#[derive(Debug, Clone, Copy)]
pub enum CueContent<'a> {
    /// Реплика простого формата (SRT, WebVTT, YouTube и т. п.).
    Text(&'a SubtitleTextCue),
    /// Реплика ASS/SSA: стиль и теги разобраны, раскладка и рисование — здесь.
    Ass(&'a AssCue),
}

/// Реплика кадра: ключ, слой и ссылка на содержимое.
#[derive(Debug, Clone, Copy)]
pub struct RenderableCue<'a> {
    /// Идентичность реплики.
    pub key: CueKey,
    /// Слой события (`Layer` ASS): для ASS-реплик больший слой рисуется поверх меньшего, а
    /// столкновения разрешаются отдельно в каждом слое. Реплики простых форматов слой не
    /// используют (их порядок задаёт запрос).
    pub layer: SubtitleLayer,
    /// Содержимое (читается только при промахе кэша).
    pub content: CueContent<'a>,
}

impl<'a> RenderableCue<'a> {
    /// Реплика простого формата.
    pub const fn text(key: CueKey, cue: &'a SubtitleTextCue) -> Self {
        Self {
            key,
            layer: SubtitleLayer::DEFAULT,
            content: CueContent::Text(cue),
        }
    }

    /// Реплика ASS на слое `layer`.
    pub const fn ass(key: CueKey, layer: SubtitleLayer, cue: &'a AssCue) -> Self {
        Self {
            key,
            layer,
            content: CueContent::Ass(cue),
        }
    }
}

/// Запрос на один кадр.
#[derive(Debug, Clone, Copy)]
pub struct RenderRequest<'a> {
    /// Активные реплики. Реплики простых форматов рисуются в порядке запроса (первая — самая
    /// нижняя; при столкновениях первая остаётся на месте, следующие сдвигаются). Реплики
    /// ASS сортируются по `(layer, ReadOrder)` и рисуются ПОСЛЕ реплик простых форматов; для
    /// равных слоя и `ReadOrder` остаётся порядок запроса.
    pub cues: &'a [RenderableCue<'a>],
    /// Холст — размер картинки видео на экране.
    pub canvas: CanvasSize,
    /// Стиль пользователя.
    pub appearance: &'a SubtitleAppearance,
    /// Язык текста для выбора региональной формы CJK при системном fallback: язык дорожки,
    /// иначе системы ([`TextLanguagePreference::track_then_system`]). Смена языка перерисует
    /// реплики, а прошлый кадр не будет переиспользован.
    pub text_language: TextLanguagePreference,
    /// Настройки рисования ASS (масштаб текста, чей стиль, размер хранения видео). Смена
    /// перерисует ASS-реплики; реплики простых форматов эти настройки не затрагивают.
    pub ass: AssRenderOptions,
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
