//! Диапазоны лимитов ресурсов секции `[subtitles]` (размер файла, число реплик, поиск файлов,
//! кэши растеризатора, встроенные шрифты и поток встроенных дорожек): значения по умолчанию,
//! минимумы и максимумы. Вынесены из `schema/subtitles.rs` без изменения значений, чтобы
//! файл схемы не раздувался; публичный путь констант не изменился (`pub use` в `schema.rs`).

/// Размер файла субтитров по умолчанию, МБ (прежняя константа 8 МиБ).
pub const DEFAULT_MAX_FILE_SIZE_MB: u16 = 8;
/// Минимальный лимит размера файла, МБ.
pub const MIN_MAX_FILE_SIZE_MB: u16 = 1;
/// Максимальный лимит размера файла, МБ.
pub const MAX_MAX_FILE_SIZE_MB: u16 = 64;

/// Реплик в файле по умолчанию.
pub const DEFAULT_MAX_CUES_PER_FILE: u32 = 100_000;
/// Минимум реплик в файле.
pub const MIN_MAX_CUES_PER_FILE: u32 = 1_000;
/// Максимум реплик в файле.
pub const MAX_MAX_CUES_PER_FILE: u32 = 1_000_000;

/// Записей папки при поиске файлов рядом, по умолчанию.
pub const DEFAULT_SIDECAR_MAX_ENTRIES_PER_FOLDER: u32 = 5_000;
/// Минимум записей папки при поиске.
pub const MIN_SIDECAR_MAX_ENTRIES_PER_FOLDER: u32 = 100;
/// Максимум записей папки при поиске.
pub const MAX_SIDECAR_MAX_ENTRIES_PER_FOLDER: u32 = 100_000;

/// Кандидатов-файлов рядом с видео, по умолчанию.
pub const DEFAULT_SIDECAR_MAX_CANDIDATES: u16 = 32;
/// Минимум кандидатов.
pub const MIN_SIDECAR_MAX_CANDIDATES: u16 = 1;
/// Максимум кандидатов.
pub const MAX_SIDECAR_MAX_CANDIDATES: u16 = 256;

/// Кэш картинок реплик по умолчанию, МБ.
pub const DEFAULT_RENDER_CACHE_MB: u16 = 32;
/// Минимальный кэш картинок, МБ.
pub const MIN_RENDER_CACHE_MB: u16 = 4;
/// Максимальный кэш картинок, МБ.
pub const MAX_RENDER_CACHE_MB: u16 = 512;

/// Кэш поиска шрифта по символу, по умолчанию.
pub const DEFAULT_FONT_LOOKUP_CACHE_CHARS: u32 = 65_536;
/// Минимум кэша поиска шрифта.
pub const MIN_FONT_LOOKUP_CACHE_CHARS: u32 = 1_024;
/// Максимум кэша поиска шрифта.
pub const MAX_FONT_LOOKUP_CACHE_CHARS: u32 = 1_048_576;

/// Символов в одной реплике, по умолчанию.
pub const DEFAULT_MAX_CHARS_PER_CUE: u32 = 2_000;
/// Минимум символов в реплике.
pub const MIN_MAX_CHARS_PER_CUE: u32 = 100;
/// Максимум символов в реплике.
pub const MAX_MAX_CHARS_PER_CUE: u32 = 20_000;

/// Одновременно показанных реплик, по умолчанию.
pub const DEFAULT_MAX_CUES_ON_SCREEN: u16 = 16;
/// Минимум одновременных реплик.
pub const MIN_MAX_CUES_ON_SCREEN: u16 = 1;
/// Максимум одновременных реплик.
pub const MAX_MAX_CUES_ON_SCREEN: u16 = 64;

/// Лимит одного вложенного шрифта по умолчанию, МБ (прежняя константа 20 МиБ в демуксере).
pub const DEFAULT_MAX_ATTACHED_FONT_MB: u16 = 20;
/// Минимальный лимит одного вложения, МБ.
pub const MIN_MAX_ATTACHED_FONT_MB: u16 = 1;
/// Максимальный лимит одного вложения, МБ.
pub const MAX_MAX_ATTACHED_FONT_MB: u16 = 256;

/// Суммарный лимит вложенных шрифтов одного видео по умолчанию, МБ (прежняя константа 100 МиБ).
pub const DEFAULT_MAX_ATTACHED_FONTS_TOTAL_MB: u16 = 100;
/// Минимальный суммарный лимит вложений, МБ.
pub const MIN_MAX_ATTACHED_FONTS_TOTAL_MB: u16 = 1;
/// Максимальный суммарный лимит вложений, МБ.
pub const MAX_MAX_ATTACHED_FONTS_TOTAL_MB: u16 = 1_024;

/// Ёмкость трубы встроенных субтитров (плеер -> интерфейс), пакетов, по умолчанию.
pub const DEFAULT_EMBEDDED_FEED_CAPACITY: u32 = 4_096;
/// Минимальная ёмкость трубы, пакетов.
pub const MIN_EMBEDDED_FEED_CAPACITY: u32 = 256;
/// Максимальная ёмкость трубы, пакетов.
pub const MAX_EMBEDDED_FEED_CAPACITY: u32 = 65_536;

/// Сколько показывать встроенную реплику без длительности, мс, по умолчанию.
pub const DEFAULT_EMBEDDED_CUE_MAX_OPEN_DURATION_MS: u32 = 5_000;
/// Минимальное время показа такой реплики, мс.
pub const MIN_EMBEDDED_CUE_MAX_OPEN_DURATION_MS: u32 = 500;
/// Максимальное время показа такой реплики, мс.
pub const MAX_EMBEDDED_CUE_MAX_OPEN_DURATION_MS: u32 = 60_000;

/// На сколько кадров вперёд поток отрисовки субтитров готовит картинки, по умолчанию.
pub const DEFAULT_PRERENDER_FRAMES: u32 = 4;
/// Минимум упреждения: 0 = готовить только текущий кадр.
pub const MIN_PRERENDER_FRAMES: u32 = 0;
/// Максимум упреждения, кадров.
pub const MAX_PRERENDER_FRAMES: u32 = 64;

/// Размер очереди готовых картинок субтитров, по умолчанию.
pub const DEFAULT_PREPARED_OVERLAYS: u32 = 8;
/// Минимальный размер очереди готовых картинок.
pub const MIN_PREPARED_OVERLAYS: u32 = 1;
/// Максимальный размер очереди готовых картинок.
pub const MAX_PREPARED_OVERLAYS: u32 = 64;
