//! Отчёт о разборе: проблемы отдельных реплик не валят файл, а попадают сюда.

/// Номер строки исходного текста, начиная с 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineNumber(u32);

impl LineNumber {
    /// Из 0-based индекса строки; значение насыщается на `u32::MAX`.
    pub(crate) fn from_zero_based_index(index: usize) -> Self {
        Self(u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX))
    }

    /// Номер строки, начиная с 1.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Что именно было не так с репликой.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseIssueKind {
    /// Время начала или конца не удалось разобрать.
    InvalidTimestamp,
    /// Конец реплики не позже её начала.
    EndNotAfterStart,
    /// В блоке нет строки со временем (`start --> end`).
    MissingTimingLine,
    /// Настройка реплики WebVTT отвергнута (реплика сохранена, настройка проигнорирована).
    InvalidCueSetting { name: CueSettingName },
    /// Настройка региона WebVTT отвергнута (остальные настройки региона сохранены).
    InvalidRegionSetting,
    /// Блок `REGION` без непустого `id`: регион невозможно сослать, он отброшен.
    RegionWithoutId,
    /// Строка `Dialogue:` ASS не соответствует раскладке `Format:` (не хватает полей
    /// или нет `Text`); строка пропущена.
    MalformedAssDialogue,
    /// Заголовок WebVTT `X-TIMESTAMP-MAP` некорректен или повторён: карта времени
    /// отсутствует (или осталась первой корректной), реплики разобраны как обычно.
    MalformedTimestampMap,
    /// ASS: в файле встретился NUL-байт; как и libass, всё после него отброшено (реплики
    /// ниже этой строки не загружены). Запись привязана к строке с NUL.
    TruncatedAtNul,
}

/// Какая настройка реплики WebVTT оказалась некорректной.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CueSettingName {
    Vertical,
    Line,
    Position,
    Size,
    Align,
    /// Некорректный синтаксис `region:` или ссылка на не объявленный регион.
    Region,
    /// Неизвестное имя или токен без пары `имя:значение`.
    Unknown,
}

/// Одна запомненная проблема с привязкой к строке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseIssue {
    pub line_number: LineNumber,
    pub kind: ParseIssueKind,
}

/// Итог разбора: что пропущено и почему.
///
/// Владеет лимитом на число запоминаемых проблем: список `issues` не растёт
/// безгранично даже на файле из миллиона битых реплик.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleParseReport {
    max_reported_issues: usize,
    skipped_cues: usize,
    empty_cues: usize,
    ignored_style_rules: usize,
    unreported_issues: usize,
    issues: Vec<ParseIssue>,
}

impl SubtitleParseReport {
    pub(crate) fn new(max_reported_issues: usize) -> Self {
        Self {
            max_reported_issues,
            skipped_cues: 0,
            empty_cues: 0,
            ignored_style_rules: 0,
            unreported_issues: 0,
            issues: Vec::new(),
        }
    }

    /// Реплика пропущена из-за ошибки: считаем её и запоминаем проблему (в пределах лимита).
    pub(crate) fn record_skipped_cue(&mut self, issue: ParseIssue) {
        self.skipped_cues = self.skipped_cues.saturating_add(1);
        self.record_issue(issue);
    }

    /// Проблема, которая НЕ приводит к пропуску реплики (например, проигнорированная
    /// настройка): запоминаем в пределах лимита, `skipped_cues` не трогаем.
    pub(crate) fn record_issue(&mut self, issue: ParseIssue) {
        if self.issues.len() < self.max_reported_issues {
            self.issues.push(issue);
        } else {
            self.unreported_issues = self.unreported_issues.saturating_add(1);
        }
    }

    /// Формат распознал правила стилей, но не поддерживает их: только счётчик
    /// (у CSS нет полезной привязки к строке, а ломать файл из-за стиля нельзя).
    pub(crate) fn record_ignored_style_rules(&mut self, count: usize) {
        self.ignored_style_rules = self.ignored_style_rules.saturating_add(count);
    }

    /// Реплика формально корректна, но без видимого текста: пропущена без ошибки.
    pub(crate) fn record_empty_cue(&mut self) {
        self.empty_cues = self.empty_cues.saturating_add(1);
    }

    /// Сколько реплик пропущено из-за ошибок.
    pub fn skipped_cues(&self) -> usize {
        self.skipped_cues
    }

    /// Сколько реплик пропущено, потому что в них нет видимого текста.
    pub fn empty_cues(&self) -> usize {
        self.empty_cues
    }

    /// Сколько правил стилей формат распознал, но не поддерживает (заполняет WebVTT).
    pub fn ignored_style_rules(&self) -> usize {
        self.ignored_style_rules
    }

    /// Проблемы, не поместившиеся в `max_reported_issues` (только счётчик).
    pub fn unreported_issues(&self) -> usize {
        self.unreported_issues
    }

    /// Запомненные проблемы в порядке появления.
    pub fn issues(&self) -> &[ParseIssue] {
        &self.issues
    }
}
