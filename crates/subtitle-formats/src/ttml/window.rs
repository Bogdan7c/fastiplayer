//! Окно действия TTML-документа, вложенного в сэмпл контейнера (ISO/IEC 14496-30).

use subtitle_core::SubtitleTime;

/// Интервал `[start, end)`, в котором документ из сэмпла считается действующим.
///
/// По ISO/IEC 14496-30 документ TTML внутри сэмпла `stpp` активен только в интервале
/// сэмпла. Поэтому разбор обрезает все реплики этим окном (пустые отбрасывает), а абзацы без
/// конца заканчиваются на `end`, а не на запасных 5 секундах. Времена - в той же шкале, что
/// и времена внутри самого документа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtmlSampleWindow {
    start: SubtitleTime,
    end: SubtitleTime,
}

impl TtmlSampleWindow {
    /// Окно `[start, end)`; `None`, если `end` не позже `start` (пустое окно бессмысленно).
    pub fn new(start: SubtitleTime, end: SubtitleTime) -> Option<Self> {
        (end > start).then_some(Self { start, end })
    }

    pub(super) fn start_micros(self) -> i64 {
        self.start.as_micros()
    }

    pub(super) fn end_micros(self) -> i64 {
        self.end.as_micros()
    }
}
