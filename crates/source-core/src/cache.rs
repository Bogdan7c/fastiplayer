use crate::{
    ByteSource, CancellationToken, Seekability, SourceFingerprint, SourceResult,
    SourceRuntimeConfig, SourceValidators,
};

/// Счётчики RAM range cache для диагностики playback/cache поведения.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheDiagnostics {
    /// Количество успешных чтений из cache.
    pub hits: u64,

    /// Количество cache miss-ов.
    pub misses: u64,

    /// Количество вытесненных ranges.
    pub evictions: u64,

    /// Текущий объём cached bytes.
    pub bytes_cached: u64,
}

/// In-memory cache byte ranges для одного media source.
#[derive(Debug, Clone)]
pub struct RamByteRangeCache {
    /// Максимальный объём cache в bytes.
    budget_bytes: u64,

    /// Суммарный размер entries.
    bytes_cached: u64,

    /// Монотонный access counter для LRU.
    next_access_id: u64,

    /// Cached ranges.
    entries: Vec<CacheEntry>,

    /// Диагностические счётчики.
    diagnostics: CacheDiagnostics,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    /// Начальный byte offset включительно.
    start: u64,

    /// Данные range-а.
    bytes: Vec<u8>,

    /// Последний access counter для LRU tie-breaker-а.
    last_access_id: u64,
}

impl CacheEntry {
    /// Возвращает offset сразу после последнего byte range-а.
    fn end_exclusive(&self) -> u64 {
        self.start.saturating_add(self.bytes.len() as u64)
    }

    /// Проверяет, полностью ли entry покрывает requested range.
    fn covers(&self, offset: u64, length: usize) -> bool {
        let requested_end = offset.saturating_add(length as u64);
        self.start <= offset && requested_end <= self.end_exclusive()
    }

    /// Возвращает расстояние entry до текущего playback offset.
    fn distance_to(&self, focus_offset: u64) -> u64 {
        if focus_offset < self.start {
            self.start - focus_offset
        } else if focus_offset >= self.end_exclusive() {
            focus_offset - self.end_exclusive()
        } else {
            0
        }
    }
}

impl RamByteRangeCache {
    /// Создаёт cache строго из runtime config, который уже получен из TOML config.
    #[must_use]
    pub fn from_source_config(source_config: &SourceRuntimeConfig) -> Self {
        Self {
            budget_bytes: source_config.memory_cache_bytes(),
            bytes_cached: 0,
            next_access_id: 1,
            entries: Vec::new(),
            diagnostics: CacheDiagnostics::default(),
        }
    }

    /// Возвращает `true`, если RAM cache включён.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.budget_bytes > 0
    }

    /// Возвращает текущие cache counters.
    #[must_use]
    pub fn diagnostics(&self) -> CacheDiagnostics {
        CacheDiagnostics {
            bytes_cached: self.bytes_cached,
            ..self.diagnostics
        }
    }

    /// Пробует скопировать requested range из cache.
    pub fn get(&mut self, offset: u64, output: &mut [u8]) -> bool {
        if !self.is_enabled() || output.is_empty() {
            return false;
        }

        let Some(entry_index) = self
            .entries
            .iter()
            .position(|entry| entry.covers(offset, output.len()))
        else {
            self.diagnostics.misses = self.diagnostics.misses.saturating_add(1);
            return false;
        };

        let access_id = self.allocate_access_id();
        let entry = &mut self.entries[entry_index];
        let entry_offset = (offset - entry.start) as usize;
        let entry_end = entry_offset + output.len();
        output.copy_from_slice(&entry.bytes[entry_offset..entry_end]);
        entry.last_access_id = access_id;
        self.diagnostics.hits = self.diagnostics.hits.saturating_add(1);
        true
    }

    /// Добавляет range в cache и применяет eviction относительно focus offset.
    pub fn insert(&mut self, offset: u64, bytes: &[u8], focus_offset: u64) {
        if !self.is_enabled() || bytes.is_empty() || bytes.len() as u64 > self.budget_bytes {
            return;
        }

        let access_id = self.allocate_access_id();
        self.entries.push(CacheEntry {
            start: offset,
            bytes: bytes.to_vec(),
            last_access_id: access_id,
        });
        self.bytes_cached = self.bytes_cached.saturating_add(bytes.len() as u64);
        self.evict_until_within_budget(focus_offset);
    }

    /// Выделяет следующий access id.
    fn allocate_access_id(&mut self) -> u64 {
        let access_id = self.next_access_id;
        self.next_access_id = self.next_access_id.saturating_add(1);
        access_id
    }

    /// Удаляет ranges, которые дальше всего от текущего offset-а и старее при равенстве.
    fn evict_until_within_budget(&mut self, focus_offset: u64) {
        while self.bytes_cached > self.budget_bytes {
            let Some(victim_index) = self.select_eviction_victim(focus_offset) else {
                break;
            };

            let victim = self.entries.swap_remove(victim_index);
            self.bytes_cached = self.bytes_cached.saturating_sub(victim.bytes.len() as u64);
            self.diagnostics.evictions = self.diagnostics.evictions.saturating_add(1);
        }
    }

    /// Выбирает range по policy range-distance, затем LRU.
    fn select_eviction_victim(&self, focus_offset: u64) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .max_by_key(|(_, entry)| {
                (
                    entry.distance_to(focus_offset),
                    u64::MAX - entry.last_access_id,
                )
            })
            .map(|(index, _)| index)
    }
}

/// ByteSource wrapper, который добавляет RAM range cache без изменения inner source.
pub struct CachedByteSource<S> {
    /// Реальный источник bytes.
    inner: S,

    /// RAM cache ranges для этого media.
    cache: RamByteRangeCache,

    /// Cursor wrapper-а, независимый от inner cursor до miss-а.
    position: u64,
}

impl<S> CachedByteSource<S>
where
    S: ByteSource,
{
    /// Создаёт cached wrapper с budget из source runtime config.
    #[must_use]
    pub fn new(inner: S, source_config: &SourceRuntimeConfig) -> Self {
        Self {
            inner,
            cache: RamByteRangeCache::from_source_config(source_config),
            position: 0,
        }
    }

    /// Возвращает текущие cache counters.
    #[must_use]
    pub fn cache_diagnostics(&self) -> CacheDiagnostics {
        self.cache.diagnostics()
    }

    /// Возвращает ссылку на wrapped source.
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S> ByteSource for CachedByteSource<S>
where
    S: ByteSource,
{
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }

        if self.cache.get(self.position, output) {
            self.position = self.position.saturating_add(output.len() as u64);
            return Ok(output.len());
        }

        self.inner.seek(self.position)?;
        let bytes_read = self.inner.read(output, cancellation)?;
        self.cache
            .insert(self.position, &output[..bytes_read], self.position);
        self.position = self.position.saturating_add(bytes_read as u64);
        Ok(bytes_read)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.position = offset;
        Ok(())
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seekability(&self) -> Seekability {
        self.inner.seekability()
    }

    fn validators(&self) -> SourceValidators {
        self.inner.validators()
    }

    fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.inner.fingerprint()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{NotSeekableReason, SourceError};

    use super::*;

    #[derive(Debug)]
    struct FakeByteSource {
        bytes: Vec<u8>,
        position: u64,
        read_count: usize,
    }

    impl FakeByteSource {
        fn new(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
                position: 0,
                read_count: 0,
            }
        }
    }

    impl ByteSource for FakeByteSource {
        fn read(
            &mut self,
            output: &mut [u8],
            _cancellation: &CancellationToken,
        ) -> SourceResult<usize> {
            self.read_count += 1;
            let start = self.position as usize;
            if start >= self.bytes.len() {
                return Ok(0);
            }

            let end = start.saturating_add(output.len()).min(self.bytes.len());
            let bytes_read = end - start;
            output[..bytes_read].copy_from_slice(&self.bytes[start..end]);
            self.position = self.position.saturating_add(bytes_read as u64);
            Ok(bytes_read)
        }

        fn seek(&mut self, offset: u64) -> SourceResult<()> {
            if offset > self.bytes.len() as u64 {
                return Err(SourceError::NotSeekable {
                    reason: NotSeekableReason::Unknown,
                });
            }
            self.position = offset;
            Ok(())
        }

        fn position(&self) -> u64 {
            self.position
        }

        fn seekability(&self) -> Seekability {
            Seekability::Seekable
        }

        fn validators(&self) -> SourceValidators {
            SourceValidators::default()
        }

        fn content_length(&self) -> Option<u64> {
            Some(self.bytes.len() as u64)
        }

        fn fingerprint(&self) -> SourceFingerprint {
            SourceFingerprint::new("fake")
        }
    }

    #[test]
    fn cache_reports_hit_and_miss() {
        let config = SourceRuntimeConfig::for_tests(
            1024,
            Duration::from_millis(10),
            Duration::from_millis(10),
        );
        let mut source = CachedByteSource::new(FakeByteSource::new(b"abcdef"), &config);
        let token = CancellationToken::never_cancelled();
        let mut output = [0_u8; 3];

        source.read(&mut output, &token).expect("first read works");
        assert_eq!(&output, b"abc");

        source.seek(0).expect("seek to cached range works");
        source.read(&mut output, &token).expect("cached read works");
        assert_eq!(&output, b"abc");

        let diagnostics = source.cache_diagnostics();
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(source.inner().read_count, 1);
    }

    #[test]
    fn cache_eviction_uses_distance_then_lru() {
        let config =
            SourceRuntimeConfig::for_tests(6, Duration::from_millis(10), Duration::from_millis(10));
        let mut cache = RamByteRangeCache::from_source_config(&config);
        let mut output = [0_u8; 2];

        cache.insert(0, b"aa", 0);
        cache.insert(10, b"bb", 10);
        cache.insert(20, b"cc", 20);
        cache.insert(30, b"dd", 30);

        assert!(!cache.get(0, &mut output));
        assert!(cache.get(10, &mut output));
        assert!(cache.get(20, &mut output));
        assert!(cache.get(30, &mut output));
        assert!(cache.diagnostics().evictions >= 1);
    }

    #[test]
    fn zero_memory_cache_disables_cache() {
        let config =
            SourceRuntimeConfig::for_tests(0, Duration::from_millis(10), Duration::from_millis(10));
        let mut source = CachedByteSource::new(FakeByteSource::new(b"abcdef"), &config);
        let token = CancellationToken::never_cancelled();
        let mut output = [0_u8; 3];

        source.read(&mut output, &token).expect("first read works");
        source.seek(0).expect("seek works");
        source.read(&mut output, &token).expect("second read works");

        let diagnostics = source.cache_diagnostics();
        assert_eq!(diagnostics.hits, 0);
        assert_eq!(diagnostics.misses, 0);
        assert_eq!(diagnostics.bytes_cached, 0);
        assert_eq!(source.inner().read_count, 2);
    }
}
