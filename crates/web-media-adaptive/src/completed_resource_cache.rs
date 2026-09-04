//! Bounded RAM-only LRU полностью дочитанных adaptive resources.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;

use bytes::Bytes;
use source_core::{HttpBoundedByteRange, HttpRangeResponseMetadata, HttpRequestTarget};

use crate::fetch::{
    AdaptiveResourcePurpose, AdaptiveResourceQueryApplication, AdaptiveResourceSecretForwarding,
};

/// Один retained `Bytes` требует slot в `Vec`; двойной slot покрывает геометрический spare capacity.
const CHUNK_STRUCTURAL_CHARGE_BYTES: usize = std::mem::size_of::<Bytes>() * 2;

/// Exact target хранит собственную строку, а также нормализованные host/path строки.
const TARGET_RETAINED_STRING_MULTIPLIER: usize = 2;

/// Запас на entry/pending owner, `Vec`/`Arc` headers, validators и allocator bookkeeping.
const RESOURCE_STRUCTURAL_CHARGE_BYTES: usize = 1_024;

/// Exact identity HTTP resource-а без раскрытия locator-а в diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CompletedResourceCacheKey {
    target: HttpRequestTarget,
    byte_range: Option<HttpBoundedByteRange>,
    maximum_body_bytes: NonZeroUsize,
    purpose: AdaptiveResourcePurpose,
    query_application: AdaptiveResourceQueryApplication,
    secret_forwarding: AdaptiveResourceSecretForwarding,
}

impl CompletedResourceCacheKey {
    /// Собирает exact key из уже validated adaptive request policy.
    pub(crate) const fn new(
        target: HttpRequestTarget,
        byte_range: Option<HttpBoundedByteRange>,
        maximum_body_bytes: NonZeroUsize,
        purpose: AdaptiveResourcePurpose,
        query_application: AdaptiveResourceQueryApplication,
        secret_forwarding: AdaptiveResourceSecretForwarding,
    ) -> Self {
        Self {
            target,
            byte_range,
            maximum_body_bytes,
            purpose,
            query_application,
            secret_forwarding,
        }
    }

    /// Возвращает консервативную heap-стоимость exact target без раскрытия его значения.
    fn retained_target_charge_bytes(&self) -> Option<usize> {
        retained_target_charge_bytes(&self.target)
    }
}

impl fmt::Debug for CompletedResourceCacheKey {
    /// Exact URL/query и secret material никогда не попадают в cache diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletedResourceCacheKey")
            .field("target", &"<redacted>")
            .field("byte_range", &self.byte_range)
            .field("maximum_body_bytes", &self.maximum_body_bytes)
            .field("purpose", &self.purpose)
            .field("query_application", &self.query_application)
            .field("secret_forwarding", &self.secret_forwarding)
            .finish()
    }
}

/// Shallow-cloned replay одного completed response.
pub(crate) struct CompletedResourceReplay {
    final_target: HttpRequestTarget,
    chunks: Arc<[Bytes]>,
    range_metadata: Option<HttpRangeResponseMetadata>,
}

impl CompletedResourceReplay {
    /// Возвращает только bounded accounting для secret-free cache marker-а.
    pub(crate) fn diagnostic_shape(&self) -> (usize, usize) {
        let body_bytes = self
            .chunks
            .iter()
            .fold(0_usize, |total, chunk| total.saturating_add(chunk.len()));
        (self.chunks.len(), body_bytes)
    }

    /// Разбирает replay на transport metadata и исходные bounded chunks.
    pub(crate) fn into_parts(
        self,
    ) -> (
        HttpRequestTarget,
        Arc<[Bytes]>,
        Option<HttpRangeResponseMetadata>,
    ) {
        (self.final_target, self.chunks, self.range_metadata)
    }
}

/// Один LRU entry; chunks разделяют immutable storage с завершённым response body.
struct CompletedResourceEntry {
    key: CompletedResourceCacheKey,
    final_target: HttpRequestTarget,
    chunks: Arc<[Bytes]>,
    charge_bytes: usize,
    range_metadata: Option<HttpRangeResponseMetadata>,
}

/// Результат reservation попытки, который не смешивает budget rejection с успешным no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletedResourceReservationOutcome {
    /// Полный requested charge зарезервирован.
    Reserved,
    /// Общий committed + pending budget не позволяет reservation.
    BudgetExceeded,
}

/// Source-local cache с общим committed/pending accounting и oldest-first eviction.
pub(crate) struct CompletedResourceCache {
    budget_bytes: usize,
    committed_charge_bytes: usize,
    pending_charge_bytes: usize,
    entries: VecDeque<CompletedResourceEntry>,
}

impl CompletedResourceCache {
    /// Создаёт cache с уже нормализованным platform-sized budget.
    pub(crate) const fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            committed_charge_bytes: 0,
            pending_charge_bytes: 0,
            entries: VecDeque::new(),
        }
    }

    /// Возвращает предел pending admission, не раскрывая entries вызывающему коду.
    pub(crate) const fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// Резервирует RAM для одного или нескольких pending admission chunks.
    ///
    /// Committed LRU entries выселяются до reservation. Другие pending owners не
    /// выселяются: при их недостаточном остатке новый admission обязан отказаться.
    pub(crate) fn reserve_pending(
        &mut self,
        additional_charge_bytes: usize,
    ) -> CompletedResourceReservationOutcome {
        let Some(next_pending_charge_bytes) = self
            .pending_charge_bytes
            .checked_add(additional_charge_bytes)
        else {
            return CompletedResourceReservationOutcome::BudgetExceeded;
        };
        if next_pending_charge_bytes > self.budget_bytes {
            return CompletedResourceReservationOutcome::BudgetExceeded;
        }

        while self
            .committed_charge_bytes
            .checked_add(next_pending_charge_bytes)
            .is_none_or(|next_total| next_total > self.budget_bytes)
        {
            let Some(evicted) = self.entries.pop_front() else {
                return CompletedResourceReservationOutcome::BudgetExceeded;
            };
            self.committed_charge_bytes = self
                .committed_charge_bytes
                .checked_sub(evicted.charge_bytes)
                .expect("committed cache charge обязан включать evicted entry");
        }

        self.pending_charge_bytes = next_pending_charge_bytes;
        CompletedResourceReservationOutcome::Reserved
    }

    /// Освобождает ровно тот charge, которым владел отменённый либо dropped admission.
    pub(crate) fn release_pending(&mut self, reservation_charge_bytes: usize) {
        self.pending_charge_bytes = self
            .pending_charge_bytes
            .checked_sub(reservation_charge_bytes)
            .expect("pending cache reservation не может освобождаться дважды");
    }

    /// Возвращает shallow replay и переносит hit в MRU-хвост.
    pub(crate) fn replay(
        &mut self,
        key: &CompletedResourceCacheKey,
    ) -> Option<CompletedResourceReplay> {
        let entry_index = self.entries.iter().position(|entry| &entry.key == key)?;
        let entry = self
            .entries
            .remove(entry_index)
            .expect("найденный LRU entry обязан существовать");
        let replay = CompletedResourceReplay {
            final_target: entry.final_target.clone(),
            chunks: entry.chunks.clone(),
            range_metadata: entry.range_metadata.clone(),
        };
        self.entries.push_back(entry);
        Some(replay)
    }

    /// Атомарно переводит полностью завершённый pending admission в committed LRU entry.
    pub(crate) fn commit_pending(
        &mut self,
        reservation_charge_bytes: usize,
        key: CompletedResourceCacheKey,
        final_target: HttpRequestTarget,
        chunks: Arc<[Bytes]>,
        range_metadata: Option<HttpRangeResponseMetadata>,
    ) {
        self.pending_charge_bytes = self
            .pending_charge_bytes
            .checked_sub(reservation_charge_bytes)
            .expect("completed admission обязан владеть pending reservation");

        if let Some(existing_index) = self.entries.iter().position(|entry| entry.key == key) {
            let existing = self
                .entries
                .remove(existing_index)
                .expect("найденный replacement entry обязан существовать");
            self.committed_charge_bytes = self
                .committed_charge_bytes
                .checked_sub(existing.charge_bytes)
                .expect("committed cache charge обязан включать replacement entry");
        }

        self.committed_charge_bytes = self
            .committed_charge_bytes
            .checked_add(reservation_charge_bytes)
            .expect("validated cache charge обязан помещаться в usize");
        debug_assert!(self.total_accounted_charge_bytes() <= self.budget_bytes);
        self.entries.push_back(CompletedResourceEntry {
            key,
            final_target,
            chunks,
            charge_bytes: reservation_charge_bytes,
            range_metadata,
        });
    }

    /// Принимает полностью завершённый response через тот же общий reservation contract.
    pub(crate) fn insert_completed(
        &mut self,
        key: CompletedResourceCacheKey,
        final_target: HttpRequestTarget,
        mut chunks: Vec<Bytes>,
        range_metadata: Option<HttpRangeResponseMetadata>,
    ) {
        chunks.retain(|chunk| !chunk.is_empty());
        let Some(body_bytes) = chunks
            .iter()
            .try_fold(0_usize, |total, chunk| total.checked_add(chunk.len()))
        else {
            return;
        };
        if body_bytes == 0 {
            return;
        }
        let Some(reservation_charge_bytes) =
            completed_entry_charge_bytes(&key, &final_target, chunks.iter().map(Bytes::len))
        else {
            return;
        };
        if self.reserve_pending(reservation_charge_bytes)
            == CompletedResourceReservationOutcome::BudgetExceeded
        {
            return;
        }
        self.commit_pending(
            reservation_charge_bytes,
            key,
            final_target,
            chunks.into(),
            range_metadata,
        );
    }

    /// Возвращает общий charge; используется invariants и cache regression tests.
    const fn total_accounted_charge_bytes(&self) -> usize {
        self.committed_charge_bytes + self.pending_charge_bytes
    }

    /// Test-only наблюдение pending reservations без раскрытия keys.
    #[cfg(test)]
    pub(crate) const fn pending_charge_bytes(&self) -> usize {
        self.pending_charge_bytes
    }

    /// Test-only наблюдение полного cache charge без раскрытия keys.
    #[cfg(test)]
    pub(crate) const fn accounted_charge_bytes(&self) -> usize {
        self.total_accounted_charge_bytes()
    }
}

/// Вычисляет initial entry/key/final-target charge до первого network chunk-а.
pub(crate) fn completed_entry_base_charge_bytes(
    key: &CompletedResourceCacheKey,
    final_target: &HttpRequestTarget,
) -> Option<usize> {
    RESOURCE_STRUCTURAL_CHARGE_BYTES
        .checked_add(key.retained_target_charge_bytes()?)?
        .checked_add(retained_target_charge_bytes(final_target)?)
}

/// Вычисляет payload + bounded structural charge одного непустого retained chunk-а.
pub(crate) fn completed_chunk_charge_bytes(chunk: &Bytes) -> Option<usize> {
    (!chunk.is_empty())
        .then(|| chunk.len().checked_add(CHUNK_STRUCTURAL_CHARGE_BYTES))
        .flatten()
}

/// Полный charge готового entry без копирования payload-а.
fn completed_entry_charge_bytes(
    key: &CompletedResourceCacheKey,
    final_target: &HttpRequestTarget,
    chunk_lengths: impl IntoIterator<Item = usize>,
) -> Option<usize> {
    chunk_lengths.into_iter().try_fold(
        completed_entry_base_charge_bytes(key, final_target)?,
        |charge_bytes, chunk_length| {
            charge_bytes.checked_add(chunk_length.checked_add(CHUNK_STRUCTURAL_CHARGE_BYTES)?)
        },
    )
}

/// Учитывает owned exact URL и отдельные normalized host/path allocations только по длине.
fn retained_target_charge_bytes(target: &HttpRequestTarget) -> Option<usize> {
    target
        .expose_secret_for_request()
        .len()
        .checked_mul(TARGET_RETAINED_STRING_MULTIPLIER)
}

impl fmt::Debug for CompletedResourceCache {
    /// Diagnostics показывают только bounds/accounting, никогда не keys.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletedResourceCache")
            .field("budget_bytes", &self.budget_bytes)
            .field("committed_charge_bytes", &self.committed_charge_bytes)
            .field("pending_charge_bytes", &self.pending_charge_bytes)
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str, purpose: AdaptiveResourcePurpose) -> CompletedResourceCacheKey {
        CompletedResourceCacheKey::new(
            HttpRequestTarget::parse_exact(format!("https://example.test{path}?token=secret"))
                .expect("cache key target"),
            None,
            NonZeroUsize::new(16).expect("body bound"),
            purpose,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
            AdaptiveResourceSecretForwarding::Suppress,
        )
    }

    fn insert(cache: &mut CompletedResourceCache, path: &str, bytes: &'static [u8]) {
        cache.insert_completed(
            key(path, AdaptiveResourcePurpose::MediaSegment),
            HttpRequestTarget::parse_exact(format!("https://cdn.example.test{path}"))
                .expect("final target"),
            vec![Bytes::from_static(bytes)],
            None,
        );
    }

    fn entry_charge(path: &str, chunks: &[Bytes]) -> usize {
        let cache_key = key(path, AdaptiveResourcePurpose::MediaSegment);
        let final_target =
            HttpRequestTarget::parse_exact(format!("https://cdn.example.test{path}"))
                .expect("entry charge final target");
        completed_entry_charge_bytes(&cache_key, &final_target, chunks.iter().map(Bytes::len))
            .expect("entry charge")
    }

    #[test]
    fn lru_hit_promotes_entry_and_byte_budget_evicts_oldest() {
        let one_entry_charge = entry_charge("/a", &[Bytes::from_static(b"aa")]);
        let mut cache = CompletedResourceCache::new(one_entry_charge * 3);
        insert(&mut cache, "/a", b"aa");
        insert(&mut cache, "/b", b"bb");
        insert(&mut cache, "/c", b"cc");

        assert!(
            cache
                .replay(&key("/a", AdaptiveResourcePurpose::MediaSegment))
                .is_some()
        );
        insert(&mut cache, "/d", b"dd");

        assert!(
            cache
                .replay(&key("/b", AdaptiveResourcePurpose::MediaSegment))
                .is_none()
        );
        assert_eq!(cache.committed_charge_bytes, one_entry_charge * 3);
        assert_eq!(cache.pending_charge_bytes, 0);
        assert_eq!(cache.entries.len(), 3);
    }

    #[test]
    fn oversized_entry_is_rejected_without_evicting_valid_entries() {
        let small_entry_charge = entry_charge("/small", &[Bytes::from_static(b"abc")]);
        let mut cache = CompletedResourceCache::new(small_entry_charge);
        insert(&mut cache, "/small", b"abc");
        insert(&mut cache, "/large", b"abcd");

        assert_eq!(cache.committed_charge_bytes, small_entry_charge);
        assert_eq!(cache.pending_charge_bytes, 0);
        assert_eq!(cache.entries.len(), 1);
        assert!(
            cache
                .replay(&key("/small", AdaptiveResourcePurpose::MediaSegment))
                .is_some()
        );
        assert!(
            cache
                .replay(&key("/large", AdaptiveResourcePurpose::MediaSegment))
                .is_none()
        );
    }

    #[test]
    fn repeated_inserts_stay_bounded_and_replacement_uses_exact_bytes() {
        let mut cache = CompletedResourceCache::new(4_096);
        for index in 0..100 {
            insert(&mut cache, &format!("/segment-{index}"), b"x");
        }

        assert!(cache.accounted_charge_bytes() <= cache.budget_bytes);
        assert_eq!(cache.pending_charge_bytes, 0);
        assert!(cache.entries.len() < 100);

        let replacement_key = key("/replacement", AdaptiveResourcePurpose::MediaSegment);
        cache.insert_completed(
            replacement_key.clone(),
            HttpRequestTarget::parse_exact("https://cdn.example.test/replacement")
                .expect("replacement target"),
            vec![Bytes::from_static(b"ab")],
            None,
        );
        cache.insert_completed(
            replacement_key,
            HttpRequestTarget::parse_exact("https://cdn.example.test/replacement")
                .expect("replacement target"),
            vec![Bytes::from_static(b"z")],
            None,
        );

        assert!(cache.accounted_charge_bytes() <= cache.budget_bytes);
        assert_eq!(cache.pending_charge_bytes, 0);
    }

    #[test]
    fn committed_and_concurrent_pending_reservations_share_one_budget() {
        let committed_chunks = [Bytes::from_static(b"committed")];
        let committed_charge = entry_charge("/committed", &committed_chunks);
        let budget_bytes = committed_charge + 800;
        let mut cache = CompletedResourceCache::new(budget_bytes);
        insert(&mut cache, "/committed", b"committed");
        assert_eq!(cache.committed_charge_bytes, committed_charge);

        assert_eq!(
            cache.reserve_pending(400),
            CompletedResourceReservationOutcome::Reserved
        );
        assert_eq!(
            cache.reserve_pending(400),
            CompletedResourceReservationOutcome::Reserved
        );
        assert_eq!(cache.accounted_charge_bytes(), budget_bytes);

        assert_eq!(
            cache.reserve_pending(1),
            CompletedResourceReservationOutcome::Reserved
        );
        assert!(cache.entries.is_empty());
        assert_eq!(cache.pending_charge_bytes(), 801);
        assert_eq!(cache.accounted_charge_bytes(), 801);

        assert_eq!(
            cache.reserve_pending(budget_bytes - 801),
            CompletedResourceReservationOutcome::Reserved
        );
        assert_eq!(cache.accounted_charge_bytes(), budget_bytes);
        assert_eq!(
            cache.reserve_pending(1),
            CompletedResourceReservationOutcome::BudgetExceeded
        );

        cache.release_pending(budget_bytes);
        assert_eq!(cache.pending_charge_bytes(), 0);
        assert_eq!(cache.accounted_charge_bytes(), 0);
    }

    #[test]
    fn empty_and_chunk_heavy_entries_cannot_escape_structural_budget() {
        let path = "/chunk-heavy";
        let chunks = vec![Bytes::from_static(b"x"); 32];
        let full_charge = entry_charge(path, &chunks);
        let mut cache = CompletedResourceCache::new(full_charge - 1);
        cache.insert_completed(
            key(path, AdaptiveResourcePurpose::MediaSegment),
            HttpRequestTarget::parse_exact("https://cdn.example.test/chunk-heavy")
                .expect("chunk-heavy final target"),
            chunks,
            None,
        );
        assert!(cache.entries.is_empty());
        assert_eq!(cache.accounted_charge_bytes(), 0);

        cache.insert_completed(
            key("/empty", AdaptiveResourcePurpose::MediaSegment),
            HttpRequestTarget::parse_exact("https://cdn.example.test/empty")
                .expect("empty final target"),
            vec![Bytes::new(); 10_000],
            None,
        );
        assert!(cache.entries.is_empty());
        assert_eq!(cache.accounted_charge_bytes(), 0);
    }

    #[test]
    fn key_distinguishes_purpose_range_bound_and_policy_without_leaking_target() {
        let base = key("/same", AdaptiveResourcePurpose::MediaSegment);
        let initialization = key("/same", AdaptiveResourcePurpose::Initialization);
        let ranged = CompletedResourceCacheKey::new(
            HttpRequestTarget::parse_exact("https://example.test/same?token=secret")
                .expect("range target"),
            Some(
                HttpBoundedByteRange::new(4, NonZeroUsize::new(8).expect("range length"))
                    .expect("valid range"),
            ),
            NonZeroUsize::new(16).expect("body bound"),
            AdaptiveResourcePurpose::MediaSegment,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
            AdaptiveResourceSecretForwarding::Suppress,
        );

        assert_ne!(base, initialization);
        assert_ne!(base, ranged);
        let debug = format!("{base:?}");
        assert!(!debug.contains("token=secret"));
        assert!(!debug.contains("example.test"));
        assert!(debug.contains("<redacted>"));
    }
}
