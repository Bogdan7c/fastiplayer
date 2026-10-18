//! Ограниченная память выбора дорожек по видео (в оперативной памяти, без диска).
//!
//! Владеет инвариантами: число записей не больше ёмкости, у каждой записи уникальная метка
//! свежести, вытеснение всегда убирает самую давнюю запись. Диск, JSON и потоки — в соседних
//! модулях.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use super::hint::RememberedTrackHint;
use super::key::VideoKey;
use super::stream_choice::RememberedStreamSubtitle;
use super::web_audio_language::RememberedWebAudioLanguage;
use super::web_choice::RememberedWebSubtitle;

/// Жёсткий потолок числа запомненных видео. Выше конфиг не пускает; файл с большим числом
/// записей считается повреждённым.
pub const TRACK_MEMORY_HARD_MAX_ENTRIES: usize = 10_000;

/// Допустимый модуль задержки субтитров, мс (как `MAX_SYNC_OFFSET_MILLIS` в приложении).
pub const MAX_SUBTITLE_SYNC_OFFSET_MILLIS: i64 = 600_000;

/// Максимум единиц пути (байт на Unix, `u16` на Windows) у запомненного файла субтитров.
/// 4096 — `PATH_MAX` Linux; более длинные пути не запоминаем.
pub const MAX_REMEMBERED_FILE_PATH_UNITS: usize = 4_096;

// ---------------------------------------------------------------------------------------
// Ёмкость
// ---------------------------------------------------------------------------------------

/// Почему ёмкость отвергнута.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackMemoryCapacityError {
    /// Ноль записей: память не могла бы помнить даже текущее видео.
    Zero,
    /// Больше [`TRACK_MEMORY_HARD_MAX_ENTRIES`].
    AboveHardMax,
}

impl fmt::Display for TrackMemoryCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Zero => "track memory capacity must be at least one entry",
            Self::AboveHardMax => "track memory capacity exceeds the hard maximum",
        })
    }
}

impl std::error::Error for TrackMemoryCapacityError {}

/// Максимальное число запомненных видео (1..=[`TRACK_MEMORY_HARD_MAX_ENTRIES`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackMemoryCapacity(usize);

impl TrackMemoryCapacity {
    /// Самая большая допустимая ёмкость.
    pub const HARD_MAX: Self = Self(TRACK_MEMORY_HARD_MAX_ENTRIES);

    /// Проверяет границы.
    pub const fn new(entries: usize) -> Result<Self, TrackMemoryCapacityError> {
        if entries == 0 {
            return Err(TrackMemoryCapacityError::Zero);
        }
        if entries > TRACK_MEMORY_HARD_MAX_ENTRIES {
            return Err(TrackMemoryCapacityError::AboveHardMax);
        }
        Ok(Self(entries))
    }

    /// Число записей.
    #[must_use]
    pub const fn entries(self) -> usize {
        self.0
    }
}

// ---------------------------------------------------------------------------------------
// Значения записи
// ---------------------------------------------------------------------------------------

/// Задержка субтитров вне диапазона ±[`MAX_SUBTITLE_SYNC_OFFSET_MILLIS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubtitleSyncOffsetOutOfRange;

impl fmt::Display for SubtitleSyncOffsetOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("subtitle sync offset is outside the supported range")
    }
}

impl std::error::Error for SubtitleSyncOffsetOutOfRange {}

/// Задержка субтитров в миллисекундах; положительная — позже, отрицательная — раньше.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubtitleSyncOffset(i64);

impl SubtitleSyncOffset {
    /// Без сдвига — значение записи по умолчанию.
    pub const ZERO: Self = Self(0);

    /// Проверяет диапазон ±[`MAX_SUBTITLE_SYNC_OFFSET_MILLIS`].
    pub const fn from_millis(millis: i64) -> Result<Self, SubtitleSyncOffsetOutOfRange> {
        if millis < -MAX_SUBTITLE_SYNC_OFFSET_MILLIS || millis > MAX_SUBTITLE_SYNC_OFFSET_MILLIS {
            return Err(SubtitleSyncOffsetOutOfRange);
        }
        Ok(Self(millis))
    }

    /// Значение в миллисекундах.
    #[must_use]
    pub const fn millis(self) -> i64 {
        self.0
    }
}

/// Почему путь к файлу субтитров нельзя запомнить.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RememberedFilePathError {
    /// Пустой путь.
    Empty,
    /// Длиннее [`MAX_REMEMBERED_FILE_PATH_UNITS`].
    TooLong,
    /// Платформа не отдаёт точные единицы пути, а путь не UTF-8.
    UnsupportedEncoding,
}

impl fmt::Display for RememberedFilePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "remembered subtitle file path is empty",
            Self::TooLong => "remembered subtitle file path is too long",
            Self::UnsupportedEncoding => "remembered subtitle file path has no exact encoding",
        })
    }
}

impl std::error::Error for RememberedFilePathError {}

/// Путь к файлу субтитров (рядом с видео или загруженному пользователем).
///
/// Инвариант: непустой, в пределах лимита, точные единицы пути доступны на этой платформе.
/// Не-UTF-8 пути Unix сохраняются побайтово, а не отвергаются.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RememberedFilePath(PathBuf);

impl RememberedFilePath {
    /// Проверяет путь.
    pub fn new(path: PathBuf) -> Result<Self, RememberedFilePathError> {
        if path.as_os_str().is_empty() {
            return Err(RememberedFilePathError::Empty);
        }
        match path_unit_count(&path) {
            Some(units) if units > MAX_REMEMBERED_FILE_PATH_UNITS => {
                Err(RememberedFilePathError::TooLong)
            }
            Some(_) => Ok(Self(path)),
            None => Err(RememberedFilePathError::UnsupportedEncoding),
        }
    }

    /// Сам путь.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Число единиц пути (`None` — платформа без точной кодировки и путь не UTF-8).
#[cfg(unix)]
fn path_unit_count(path: &Path) -> Option<usize> {
    use std::os::unix::ffi::OsStrExt;
    Some(path.as_os_str().as_bytes().len())
}

#[cfg(windows)]
fn path_unit_count(path: &Path) -> Option<usize> {
    use std::os::windows::ffi::OsStrExt;
    Some(path.as_os_str().encode_wide().count())
}

#[cfg(not(any(unix, windows)))]
fn path_unit_count(path: &Path) -> Option<usize> {
    path.to_str().map(str::len)
}

/// Что пользователь выбрал для субтитров этого видео.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RememberedSubtitleChoice {
    /// Субтитры выключены явно (отличается от «не выбирал»).
    Off,
    /// Встроенная дорожка.
    Embedded(RememberedTrackHint),
    /// Внешний файл.
    File(RememberedFilePath),
    /// Web-дорожка (язык + вид); схема файла 2 и новее.
    Web(RememberedWebSubtitle),
    /// Дорожка потока (HLS-рендиция): язык + название + forced; схема файла 3 и новее.
    Stream(RememberedStreamSubtitle),
}

/// Метка свежести записи: чем больше, тем позже запись использовалась.
///
/// Монотонный счётчик, а не время: детерминирован в тестах и не страдает от скачков часов.
/// Внутри одной памяти метки уникальны, поэтому порядок вытеснения однозначен.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecencyStamp(u64);

impl RecencyStamp {
    /// Значение для файла.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Восстановление из файла.
    pub(super) const fn from_persisted(value: u64) -> Self {
        Self(value)
    }
}

/// Запомненный выбор для одного видео.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackMemoryEntry {
    last_used: RecencyStamp,
    audio: Option<RememberedTrackHint>,
    subtitles: Option<RememberedSubtitleChoice>,
    subtitle_sync_offset: SubtitleSyncOffset,
    web_audio_language: Option<RememberedWebAudioLanguage>,
}

impl TrackMemoryEntry {
    /// Метка последнего использования.
    #[must_use]
    pub const fn last_used(&self) -> RecencyStamp {
        self.last_used
    }

    /// Выбранная звуковая дорожка; `None` — пользователь не выбирал.
    #[must_use]
    pub fn audio(&self) -> Option<&RememberedTrackHint> {
        self.audio.as_ref()
    }

    /// Выбор субтитров; `None` — пользователь не выбирал.
    #[must_use]
    pub fn subtitles(&self) -> Option<&RememberedSubtitleChoice> {
        self.subtitles.as_ref()
    }

    /// Задержка субтитров (0, если не меняли).
    #[must_use]
    pub const fn subtitle_sync_offset(&self) -> SubtitleSyncOffset {
        self.subtitle_sync_offset
    }

    /// Язык звука web-видео, выбранный пользователем; `None` — не выбирал.
    #[must_use]
    pub fn web_audio_language(&self) -> Option<&RememberedWebAudioLanguage> {
        self.web_audio_language.as_ref()
    }

    /// Запись из файла (метка приходит оттуда же).
    pub(super) fn restored(
        last_used: RecencyStamp,
        audio: Option<RememberedTrackHint>,
        subtitles: Option<RememberedSubtitleChoice>,
        subtitle_sync_offset: SubtitleSyncOffset,
        web_audio_language: Option<RememberedWebAudioLanguage>,
    ) -> Self {
        Self {
            last_used,
            audio,
            subtitles,
            subtitle_sync_offset,
            web_audio_language,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Исходы операций
// ---------------------------------------------------------------------------------------

/// Итог `remember_*`: различаем «ничего не изменилось» и «записали».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RememberOutcome {
    /// Такое же значение уже было; запись и её свежесть не тронуты, писать на диск нечего.
    Unchanged,
    /// Значение сохранено, запись стала самой свежей; `evicted_entries` давних записей
    /// вытеснено ради ёмкости.
    Stored { evicted_entries: usize },
}

/// Итог `touch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchOutcome {
    /// Свежесть записи обновлена.
    Touched,
    /// Для этого видео записи нет; память не изменилась.
    NotRemembered,
}

/// Файл с нарушенными инвариантами записей (при восстановлении).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestoreError {
    /// Записей больше жёсткого потолка.
    TooManyEntries,
    /// Один и тот же видео-ключ дважды.
    DuplicateVideoKey,
    /// Две записи с одной меткой свежести.
    DuplicateRecencyStamp,
}

// ---------------------------------------------------------------------------------------
// Сама память
// ---------------------------------------------------------------------------------------

/// Память выбора дорожек: видео-ключ -> запись, не больше `capacity` записей.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackMemory {
    capacity: TrackMemoryCapacity,
    entries: HashMap<VideoKey, TrackMemoryEntry>,
    /// Метка, которую получит следующая «использованная» запись; больше всех существующих.
    next_stamp: u64,
}

impl TrackMemory {
    /// Пустая память.
    #[must_use]
    pub fn new(capacity: TrackMemoryCapacity) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            next_stamp: 1,
        }
    }

    /// Текущая ёмкость.
    #[must_use]
    pub const fn capacity(&self) -> TrackMemoryCapacity {
        self.capacity
    }

    /// Сколько видео помним.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Пуста ли память.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Запись для видео, если есть. Не обновляет свежесть (для этого есть [`Self::touch`]).
    #[must_use]
    pub fn entry_for(&self, key: &VideoKey) -> Option<&TrackMemoryEntry> {
        self.entries.get(key)
    }

    /// Запоминает звуковую дорожку, выбранную пользователем.
    pub fn remember_audio(&mut self, key: VideoKey, audio: RememberedTrackHint) -> RememberOutcome {
        self.update_entry(key, |entry| {
            if entry.audio.as_ref() == Some(&audio) {
                return false;
            }
            entry.audio = Some(audio);
            true
        })
    }

    /// Запоминает язык звука web-видео, выбранный пользователем (оригинал тоже: так возврат
    /// на оригинал перекрывает ранее запомненный дубляж).
    pub fn remember_web_audio_language(
        &mut self,
        key: VideoKey,
        language: RememberedWebAudioLanguage,
    ) -> RememberOutcome {
        self.update_entry(key, |entry| {
            if entry.web_audio_language.as_ref() == Some(&language) {
                return false;
            }
            entry.web_audio_language = Some(language);
            true
        })
    }

    /// Запоминает выбор субтитров (включая явное «выключены»).
    pub fn remember_subtitles(
        &mut self,
        key: VideoKey,
        choice: RememberedSubtitleChoice,
    ) -> RememberOutcome {
        self.update_entry(key, |entry| {
            if entry.subtitles.as_ref() == Some(&choice) {
                return false;
            }
            entry.subtitles = Some(choice);
            true
        })
    }

    /// Запоминает задержку субтитров. Нулевая задержка для видео без записи не создаёт запись:
    /// «не менял» и «вернул в 0» без другого выбора ничего не стоят хранить.
    pub fn remember_sync_offset(
        &mut self,
        key: VideoKey,
        offset: SubtitleSyncOffset,
    ) -> RememberOutcome {
        if offset == SubtitleSyncOffset::ZERO && !self.entries.contains_key(&key) {
            return RememberOutcome::Unchanged;
        }
        self.update_entry(key, |entry| {
            if entry.subtitle_sync_offset == offset {
                return false;
            }
            entry.subtitle_sync_offset = offset;
            true
        })
    }

    /// Отмечает видео использованным (например, при открытии), защищая запись от вытеснения.
    pub fn touch(&mut self, key: &VideoKey) -> TouchOutcome {
        if !self.entries.contains_key(key) {
            return TouchOutcome::NotRemembered;
        }
        let stamp = self.allocate_stamp();
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_used = stamp;
        }
        TouchOutcome::Touched
    }

    /// Меняет ёмкость; при уменьшении вытесняет самые давние записи. Возвращает, сколько
    /// записей вытеснено.
    pub fn set_capacity(&mut self, capacity: TrackMemoryCapacity) -> usize {
        self.capacity = capacity;
        self.evict_oldest_beyond_capacity()
    }

    /// Подводит счётчик меток к границе u64 — проверка перенумерации без 2^64 вызовов.
    #[cfg(test)]
    pub(super) fn exhaust_stamps_for_tests(&mut self) {
        self.next_stamp = u64::MAX - 1;
    }

    /// Записи от самой давней к самой свежей — детерминированный порядок для файла.
    pub(super) fn entries_oldest_first(&self) -> Vec<(VideoKey, &TrackMemoryEntry)> {
        let mut ordered: Vec<(VideoKey, &TrackMemoryEntry)> = self
            .entries
            .iter()
            .map(|(key, entry)| (*key, entry))
            .collect();
        ordered.sort_by_key(|(_, entry)| entry.last_used);
        ordered
    }

    /// Собирает память из восстановленных записей, проверяя инварианты. Если записей больше
    /// ёмкости, лишние давние сразу вытесняются.
    pub(super) fn restore(
        capacity: TrackMemoryCapacity,
        restored: Vec<(VideoKey, TrackMemoryEntry)>,
    ) -> Result<Self, RestoreError> {
        if restored.len() > TRACK_MEMORY_HARD_MAX_ENTRIES {
            return Err(RestoreError::TooManyEntries);
        }
        let mut memory = Self::new(capacity);
        let mut highest_stamp = 0_u64;
        let mut used_stamps = std::collections::HashSet::with_capacity(restored.len());
        for (key, entry) in restored {
            if !used_stamps.insert(entry.last_used) {
                return Err(RestoreError::DuplicateRecencyStamp);
            }
            highest_stamp = highest_stamp.max(entry.last_used.get());
            if memory.entries.insert(key, entry).is_some() {
                return Err(RestoreError::DuplicateVideoKey);
            }
        }
        memory.next_stamp = highest_stamp.saturating_add(1);
        memory.renumber_if_stamps_exhausted();
        memory.evict_oldest_beyond_capacity();
        Ok(memory)
    }

    /// Общий путь `remember_*`: создаёт запись при необходимости, применяет изменение и, только
    /// если оно что-то поменяло, делает запись самой свежей и вытесняет лишнее.
    fn update_entry(
        &mut self,
        key: VideoKey,
        apply_change: impl FnOnce(&mut TrackMemoryEntry) -> bool,
    ) -> RememberOutcome {
        let stamp = self.allocate_stamp();
        let entry = self.entries.entry(key).or_insert_with(|| TrackMemoryEntry {
            last_used: stamp,
            audio: None,
            subtitles: None,
            subtitle_sync_offset: SubtitleSyncOffset::ZERO,
            web_audio_language: None,
        });
        if !apply_change(entry) {
            // Сюда попадает только существующая запись: для новой пустой записи любое
            // значение — изменение (нулевую задержку без записи отсекает `remember_sync_offset`).
            // Метка при этом зря потрачена, но монотонность и уникальность не страдают.
            return RememberOutcome::Unchanged;
        }
        entry.last_used = stamp;
        RememberOutcome::Stored {
            evicted_entries: self.evict_oldest_beyond_capacity(),
        }
    }

    /// Выдаёт следующую уникальную метку; при исчерпании u64 перенумеровывает записи.
    fn allocate_stamp(&mut self) -> RecencyStamp {
        self.renumber_if_stamps_exhausted();
        let stamp = RecencyStamp(self.next_stamp);
        self.next_stamp += 1;
        stamp
    }

    /// Практически недостижимо (u64), но без паники и без нарушения порядка: сохраняет
    /// относительный порядок записей, сжимая метки до 1..=n.
    fn renumber_if_stamps_exhausted(&mut self) {
        if self.next_stamp < u64::MAX - 1 {
            return;
        }
        let mut keys_oldest_first: Vec<VideoKey> = self
            .entries_oldest_first()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        let mut next = 1_u64;
        for key in keys_oldest_first.drain(..) {
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.last_used = RecencyStamp(next);
                next += 1;
            }
        }
        self.next_stamp = next;
    }

    /// Убирает самые давние записи, пока их больше ёмкости. Возвращает число вытесненных.
    fn evict_oldest_beyond_capacity(&mut self) -> usize {
        let excess = self.entries.len().saturating_sub(self.capacity.entries());
        if excess == 0 {
            return 0;
        }
        let victims: Vec<VideoKey> = self
            .entries_oldest_first()
            .into_iter()
            .take(excess)
            .map(|(key, _)| key)
            .collect();
        for key in &victims {
            self.entries.remove(key);
        }
        victims.len()
    }
}
