//! First-class top-level entries и compound ownership invariants.

use std::collections::HashSet;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};

use crate::{
    CachedPlaylistMetadata, PlaylistCompoundDurablePayload, PlaylistItem, PlaylistItemDraft,
    PlaylistItemId, PlaylistLocator,
};

/// Непрозрачная стабильная идентичность compound group.
///
/// Group ID описывает structural top-level entry. Он не является playable Item ID
/// и никогда не передаётся player boundary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaylistCompoundGroupId(NonZeroU64);

impl PlaylistCompoundGroupId {
    /// Восстанавливает Group ID из persistence-facing fixed-width значения.
    pub fn from_persistence_value(
        value: u64,
    ) -> Result<Self, PlaylistCompoundGroupIdPersistenceError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(PlaylistCompoundGroupIdPersistenceError::ReservedZero)
    }

    /// Возвращает fixed-width значение только для persistence/correlation boundary.
    pub const fn expose_value_for_persistence(self) -> u64 {
        self.0.get()
    }

    /// Создаёт Group ID из уже проверенного allocator-ом значения.
    const fn from_non_zero(value: NonZeroU64) -> Self {
        Self(value)
    }
}

impl fmt::Debug for PlaylistCompoundGroupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PlaylistCompoundGroupId")
            .field(&self.0.get())
            .finish()
    }
}

impl fmt::Display for PlaylistCompoundGroupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "playlist-compound-group-{}", self.0.get())
    }
}

/// Ошибка преобразования persistence-значения в Group ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistCompoundGroupIdPersistenceError {
    /// Ноль зарезервирован как отсутствие identity.
    ReservedZero,
}

impl fmt::Display for PlaylistCompoundGroupIdPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("нулевой PlaylistCompoundGroupId зарезервирован")
    }
}

impl std::error::Error for PlaylistCompoundGroupIdPersistenceError {}

/// Persistence-facing snapshot следующего ещё не выданного Group ID.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NextPlaylistCompoundGroupId(NonZeroU64);

impl NextPlaylistCompoundGroupId {
    /// Возвращает первый watermark новой lineage без allocation side effect.
    pub const fn initial() -> Self {
        Self(NonZeroU64::MIN)
    }

    /// Валидирует сохранённый non-zero high-watermark.
    pub fn from_persistence_value(value: u64) -> Result<Self, CompoundGroupAllocatorRestoreError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(CompoundGroupAllocatorRestoreError::ReservedZeroWatermark)
    }

    /// Возвращает snapshot-значение только для persistence DTO.
    pub const fn expose_value_for_persistence(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Debug for NextPlaylistCompoundGroupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("NextPlaylistCompoundGroupId")
            .field(&self.0.get())
            .finish()
    }
}

impl fmt::Display for NextPlaylistCompoundGroupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "next-playlist-compound-group-{}", self.0.get())
    }
}

/// Владелец независимого монотонного Group ID high-watermark.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistCompoundGroupIdAllocator {
    next_group_id: NonZeroU64,
}

impl PlaylistCompoundGroupIdAllocator {
    /// Создаёт новую lineage, где первый будущий Group ID равен 1.
    pub const fn initial() -> Self {
        Self {
            next_group_id: NonZeroU64::MIN,
        }
    }

    /// Восстанавливает allocator из watermark, строго большего всех Group IDs.
    pub fn restore(
        next_group_id: NextPlaylistCompoundGroupId,
        restored_group_ids: &[PlaylistCompoundGroupId],
    ) -> Result<Self, CompoundGroupAllocatorRestoreError> {
        if let Some(maximum_group_id) = restored_group_ids.iter().copied().max()
            && next_group_id.0 <= maximum_group_id.0
        {
            return Err(
                CompoundGroupAllocatorRestoreError::WatermarkNotAboveRestoredIds {
                    next_group_id,
                    maximum_group_id,
                },
            );
        }

        Ok(Self {
            next_group_id: next_group_id.0,
        })
    }

    /// Снимает persistence snapshot без выдачи identity.
    pub const fn snapshot(&self) -> NextPlaylistCompoundGroupId {
        NextPlaylistCompoundGroupId(self.next_group_id)
    }

    /// Полностью проверяет будущий диапазон без изменения high-watermark.
    pub(crate) fn preflight_allocation(
        &self,
        group_count: usize,
        existing_group_ids: &HashSet<PlaylistCompoundGroupId>,
    ) -> Result<CompoundGroupIdAllocationPlan, CompoundGroupIdAllocationError> {
        if group_count == 0 {
            return Ok(CompoundGroupIdAllocationPlan {
                allocated_group_ids: Vec::new(),
                next_group_id_after_commit: self.next_group_id,
            });
        }

        let group_count_u64 = u64::try_from(group_count)
            .map_err(|_| CompoundGroupIdAllocationError::ArithmeticExhausted)?;
        let next_group_id_after_commit = self
            .next_group_id
            .get()
            .checked_add(group_count_u64)
            .and_then(NonZeroU64::new)
            .ok_or(CompoundGroupIdAllocationError::ArithmeticExhausted)?;
        let mut allocated_group_ids = Vec::with_capacity(group_count);

        for offset in 0..group_count_u64 {
            let raw_group_id = self
                .next_group_id
                .get()
                .checked_add(offset)
                .and_then(NonZeroU64::new)
                .ok_or(CompoundGroupIdAllocationError::ArithmeticExhausted)?;
            let group_id = PlaylistCompoundGroupId::from_non_zero(raw_group_id);

            if existing_group_ids.contains(&group_id) {
                return Err(CompoundGroupIdAllocationError::Collision { group_id });
            }

            allocated_group_ids.push(group_id);
        }

        Ok(CompoundGroupIdAllocationPlan {
            allocated_group_ids,
            next_group_id_after_commit,
        })
    }

    /// Публикует high-watermark только в общем queue commit point.
    pub(crate) fn commit_allocation(&mut self, plan: &CompoundGroupIdAllocationPlan) {
        self.next_group_id = plan.next_group_id_after_commit;
    }
}

impl Default for PlaylistCompoundGroupIdAllocator {
    fn default() -> Self {
        Self::initial()
    }
}

impl fmt::Debug for PlaylistCompoundGroupIdAllocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistCompoundGroupIdAllocator")
            .field("next_group_id", &self.next_group_id.get())
            .finish()
    }
}

/// Ошибка восстановления Group ID allocator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompoundGroupAllocatorRestoreError {
    /// Нулевой watermark не может породить non-zero identity.
    ReservedZeroWatermark,
    /// Watermark обязан быть строго выше каждого restored Group ID.
    WatermarkNotAboveRestoredIds {
        /// Отклонённый allocator watermark.
        next_group_id: NextPlaylistCompoundGroupId,
        /// Наибольший восстановленный Group ID.
        maximum_group_id: PlaylistCompoundGroupId,
    },
}

impl fmt::Display for CompoundGroupAllocatorRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedZeroWatermark => {
                formatter.write_str("нулевой compound Group ID watermark зарезервирован")
            }
            Self::WatermarkNotAboveRestoredIds {
                next_group_id,
                maximum_group_id,
            } => write!(
                formatter,
                "compound Group ID watermark {next_group_id} не выше restored ID {maximum_group_id}"
            ),
        }
    }
}

impl std::error::Error for CompoundGroupAllocatorRestoreError {}

/// Полностью проверенный, но ещё не опубликованный диапазон Group IDs.
pub(crate) struct CompoundGroupIdAllocationPlan {
    /// IDs в canonical порядке compound drafts.
    pub(crate) allocated_group_ids: Vec<PlaylistCompoundGroupId>,
    /// High-watermark, публикуемый только после общего commit.
    next_group_id_after_commit: NonZeroU64,
}

/// Внутренняя причина отказа Group ID allocation preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompoundGroupIdAllocationError {
    /// Fixed-width checked arithmetic не может представить весь диапазон.
    ArithmeticExhausted,
    /// Будущий Group ID уже присутствует в canonical queue.
    Collision {
        /// Identity, конфликтующая с committed group.
        group_id: PlaylistCompoundGroupId,
    },
}

/// Structural identity одного top-level queue entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlaylistEntryId {
    /// Самостоятельный playable item.
    Single(PlaylistItemId),
    /// Compound header, владеющий одной или несколькими playable parts.
    Compound(PlaylistCompoundGroupId),
}

/// Ненулевой source-order ordinal compound part.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaylistCompoundPartOrdinal(NonZeroU32);

impl PlaylistCompoundPartOrdinal {
    /// Возвращает человекочитаемый one-based ordinal.
    pub const fn one_based(self) -> u32 {
        self.0.get()
    }

    /// Строит ordinal из bounded zero-based позиции.
    pub(crate) fn from_zero_based(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }
}

impl fmt::Debug for PlaylistCompoundPartOrdinal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PlaylistCompoundPartOrdinal")
            .field(&self.0.get())
            .finish()
    }
}

impl fmt::Display for PlaylistCompoundPartOrdinal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "compound-part-{}", self.0.get())
    }
}

/// Immutable ownership proof одной playable part.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlaylistCompoundMembership {
    group_id: PlaylistCompoundGroupId,
    ordinal: PlaylistCompoundPartOrdinal,
}

impl PlaylistCompoundMembership {
    /// Возвращает owning top-level Group ID.
    pub const fn group_id(self) -> PlaylistCompoundGroupId {
        self.group_id
    }

    /// Возвращает immutable source-order ordinal.
    pub const fn ordinal(self) -> PlaylistCompoundPartOrdinal {
        self.ordinal
    }
}

/// ID-less compound draft с root summary/provenance и retained playable parts.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistCompoundGroupDraft {
    provenance_locator: PlaylistLocator,
    cached_summary: CachedPlaylistMetadata,
    parts: Vec<PlaylistItemDraft>,
    durable_payload: Option<PlaylistCompoundDurablePayload>,
}

impl PlaylistCompoundGroupDraft {
    /// Создаёт compound draft, отвергая zero-part group до allocation boundary.
    pub fn new(
        provenance_locator: PlaylistLocator,
        cached_summary: CachedPlaylistMetadata,
        parts: Vec<PlaylistItemDraft>,
    ) -> Result<Self, EmptyPlaylistCompoundDraft> {
        if parts.is_empty() {
            return Err(EmptyPlaylistCompoundDraft);
        }

        Ok(Self {
            provenance_locator,
            cached_summary,
            parts,
            durable_payload: None,
        })
    }

    /// Возвращает безопасную root provenance identity.
    pub const fn provenance_locator(&self) -> &PlaylistLocator {
        &self.provenance_locator
    }

    /// Возвращает group-level cached summary.
    pub const fn cached_summary(&self) -> &CachedPlaylistMetadata {
        &self.cached_summary
    }

    /// Возвращает число retained playable parts.
    pub const fn retained_part_count(&self) -> usize {
        self.parts.len()
    }

    /// Итерирует ID-less parts в source order для preview/domain inspection.
    pub fn parts(
        &self,
    ) -> impl ExactSizeIterator<Item = &PlaylistItemDraft> + DoubleEndedIterator + '_ {
        self.parts.iter()
    }

    /// Прикрепляет validated durable group payload без изменения part order.
    pub fn with_durable_payload(mut self, payload: PlaylistCompoundDurablePayload) -> Self {
        self.durable_payload = Some(payload);
        self
    }

    /// Возвращает optional durable group payload для persistence.
    pub const fn durable_payload(&self) -> Option<&PlaylistCompoundDurablePayload> {
        self.durable_payload.as_ref()
    }
}

impl fmt::Debug for PlaylistCompoundGroupDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistCompoundGroupDraft")
            .field("provenance_locator", &self.provenance_locator)
            .field("cached_summary", &self.cached_summary)
            .field("part_count", &self.parts.len())
            .finish()
    }
}

/// Typed issue пустого compound draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmptyPlaylistCompoundDraft;

impl fmt::Display for EmptyPlaylistCompoundDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("compound draft обязан содержать хотя бы одну retained part")
    }
}

impl std::error::Error for EmptyPlaylistCompoundDraft {}

/// ID-less top-level queue draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaylistEntryDraft {
    /// Самостоятельный playable item.
    Single(PlaylistItemDraft),
    /// First-class compound group.
    Compound(PlaylistCompoundGroupDraft),
}

impl PlaylistEntryDraft {
    /// Возвращает retained Item ID demand этого top-level draft.
    pub const fn retained_item_count(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Compound(group) => group.retained_part_count(),
        }
    }

    /// Сообщает, нужен ли draft-у новый Group ID.
    pub const fn is_compound(&self) -> bool {
        matches!(self, Self::Compound(_))
    }
}

impl From<PlaylistItemDraft> for PlaylistEntryDraft {
    fn from(draft: PlaylistItemDraft) -> Self {
        Self::Single(draft)
    }
}

/// Committed playable part, принадлежащая ровно одной compound group.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistCompoundPart {
    membership: PlaylistCompoundMembership,
    item: PlaylistItem,
}

impl PlaylistCompoundPart {
    /// Возвращает immutable membership/ordinal.
    pub const fn membership(&self) -> PlaylistCompoundMembership {
        self.membership
    }

    /// Возвращает subordinate playable item.
    pub const fn item(&self) -> &PlaylistItem {
        &self.item
    }

    /// Возвращает mutable item только canonical storage owner-у.
    pub(crate) const fn item_mut(&mut self) -> &mut PlaylistItem {
        &mut self.item
    }
}

impl fmt::Debug for PlaylistCompoundPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistCompoundPart")
            .field("membership", &self.membership)
            .field("item", &self.item)
            .finish()
    }
}

/// First-class committed compound group.
#[derive(Clone, PartialEq, Eq)]
pub struct PlaylistCompoundGroup {
    group_id: PlaylistCompoundGroupId,
    provenance_locator: PlaylistLocator,
    cached_summary: CachedPlaylistMetadata,
    parts: Box<[PlaylistCompoundPart]>,
    durable_payload: Option<PlaylistCompoundDurablePayload>,
}

impl PlaylistCompoundGroup {
    /// Возвращает stable structural Group ID.
    pub const fn group_id(&self) -> PlaylistCompoundGroupId {
        self.group_id
    }

    /// Возвращает root provenance locator без изменения secret policy locator-а.
    pub const fn provenance_locator(&self) -> &PlaylistLocator {
        &self.provenance_locator
    }

    /// Возвращает group-level cached summary.
    pub const fn cached_summary(&self) -> &CachedPlaylistMetadata {
        &self.cached_summary
    }

    /// Возвращает optional durable group payload без service dependency.
    pub const fn durable_payload(&self) -> Option<&PlaylistCompoundDurablePayload> {
        self.durable_payload.as_ref()
    }

    /// Итерирует ordered retained parts без раскрытия storage mutation.
    pub fn parts(
        &self,
    ) -> impl ExactSizeIterator<Item = &PlaylistCompoundPart> + DoubleEndedIterator + '_ {
        self.parts.iter()
    }

    /// Возвращает число retained playable parts.
    pub const fn retained_part_count(&self) -> usize {
        self.parts.len()
    }

    /// Возвращает storage slice только внутреннему borrowed iterator owner.
    pub(crate) const fn parts_slice(&self) -> &[PlaylistCompoundPart] {
        &self.parts
    }

    /// Возвращает mutable parts slice только canonical storage owner-у.
    pub(crate) fn parts_slice_mut(&mut self) -> &mut [PlaylistCompoundPart] {
        &mut self.parts
    }

    /// Строит committed group из полностью preflighted identity ranges.
    pub(crate) fn from_draft(
        draft: PlaylistCompoundGroupDraft,
        group_id: PlaylistCompoundGroupId,
        part_item_ids: &[PlaylistItemId],
    ) -> Self {
        debug_assert_eq!(draft.parts.len(), part_item_ids.len());
        let parts = draft
            .parts
            .into_iter()
            .zip(part_item_ids.iter().copied())
            .enumerate()
            .map(|(part_index, (part_draft, item_id))| {
                let ordinal = PlaylistCompoundPartOrdinal::from_zero_based(part_index)
                    .expect("queue capacity guarantees a representable compound part ordinal");
                PlaylistCompoundPart {
                    membership: PlaylistCompoundMembership { group_id, ordinal },
                    item: part_draft.into_item(item_id),
                }
            })
            .collect();

        Self {
            group_id,
            provenance_locator: draft.provenance_locator,
            cached_summary: draft.cached_summary,
            parts,
            durable_payload: draft.durable_payload,
        }
    }
}

impl fmt::Debug for PlaylistCompoundGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistCompoundGroup")
            .field("group_id", &self.group_id)
            .field("provenance_locator", &self.provenance_locator)
            .field("cached_summary", &self.cached_summary)
            .field("parts", &self.parts)
            .finish()
    }
}

/// Canonical first-class top-level queue entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaylistEntry {
    /// Самостоятельный playable item.
    Single(PlaylistItem),
    /// Compound group с ordered subordinate playable parts.
    Compound(Box<PlaylistCompoundGroup>),
}

impl PlaylistEntry {
    /// Возвращает structural top-level identity.
    pub const fn entry_id(&self) -> PlaylistEntryId {
        match self {
            Self::Single(item) => PlaylistEntryId::Single(item.item_id()),
            Self::Compound(group) => PlaylistEntryId::Compound(group.group_id()),
        }
    }

    /// Возвращает число subordinate playable Item IDs.
    pub const fn retained_item_count(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Compound(group) => group.retained_part_count(),
        }
    }

    /// Возвращает single item, не превращая compound part в structural entry.
    pub const fn as_single(&self) -> Option<&PlaylistItem> {
        match self {
            Self::Single(item) => Some(item),
            Self::Compound(_) => None,
        }
    }

    /// Возвращает compound group без раскрытия mutable storage.
    pub const fn as_compound(&self) -> Option<&PlaylistCompoundGroup> {
        match self {
            Self::Single(_) => None,
            Self::Compound(group) => Some(group),
        }
    }

    /// Ищет playable identity внутри entry, не меняя structural identity.
    #[cfg(test)]
    pub(crate) fn item(&self, item_id: PlaylistItemId) -> Option<&PlaylistItem> {
        match self {
            Self::Single(item) if item.item_id() == item_id => Some(item),
            Self::Single(_) => None,
            Self::Compound(group) => group
                .parts_slice()
                .iter()
                .map(PlaylistCompoundPart::item)
                .find(|item| item.item_id() == item_id),
        }
    }

    /// Применяет metadata-only mutation ко всем subordinate playable items.
    pub(crate) fn for_each_playable_item_mut(
        &mut self,
        mutate_item: &mut impl FnMut(&mut PlaylistItem),
    ) {
        match self {
            Self::Single(item) => mutate_item(item),
            Self::Compound(group) => {
                for part in group.parts_slice_mut() {
                    mutate_item(part.item_mut());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_group_allocator_preflights_without_publishing_identity() {
        let allocator = PlaylistCompoundGroupIdAllocator::initial();
        let plan = allocator
            .preflight_allocation(1, &HashSet::new())
            .expect("first Group ID must fit");

        assert_eq!(allocator.snapshot().expose_value_for_persistence(), 1);
        assert_eq!(
            plan.allocated_group_ids[0].expose_value_for_persistence(),
            1
        );
    }

    #[test]
    fn group_allocator_collision_rejects_whole_plan_without_burn() {
        let allocator = PlaylistCompoundGroupIdAllocator::initial();
        let colliding_id =
            PlaylistCompoundGroupId::from_persistence_value(2).expect("non-zero fixture Group ID");
        let existing_group_ids = HashSet::from([colliding_id]);

        let outcome = allocator.preflight_allocation(2, &existing_group_ids);

        assert_eq!(
            outcome.err(),
            Some(CompoundGroupIdAllocationError::Collision {
                group_id: colliding_id
            })
        );
        assert_eq!(allocator.snapshot().expose_value_for_persistence(), 1);
    }
}
