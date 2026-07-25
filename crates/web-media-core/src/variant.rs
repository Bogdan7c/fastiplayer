//! Нейтральный каталог независимых video/audio component variants.
//!
//! Модуль хранит только immutable value-контракты. Provider, network runtime,
//! player, UI и алгоритм переоткрытия остаются за пределами этого crate.

use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroUsize;

use crate::{
    AudioTrackDescriptor, ExactSelectionIdentity, PreferredHeightPolicy, SemanticIdentity,
    SourceIdentity, VideoTrackDescriptor,
};

/// Максимальный размер opaque variant key в UTF-8 байтах.
pub const MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES: usize = 256;

/// Жёсткий safety ceiling для одного catalog безотносительно caller budget.
pub const MAX_COMPONENT_VARIANT_CATALOG_ENTRIES: usize = 4_096;

/// Ось независимого component variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComponentKind {
    /// Вариант video track.
    Video,
    /// Вариант audio track.
    Audio,
}

/// Ошибка построения opaque variant key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentVariantKeyError {
    /// Exact key не может быть пустым.
    Empty,
    /// Control characters запрещены, чтобы key нельзя было безопасно спутать в diagnostics.
    ContainsControlCharacter,
    /// UTF-8 представление превышает общий compatibility bound.
    TooLong {
        /// Фактическое число UTF-8 байт.
        provided_bytes: usize,
        /// Разрешённое число UTF-8 байт.
        maximum_bytes: usize,
    },
}

impl fmt::Display for ComponentVariantKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("component variant key пуст"),
            Self::ContainsControlCharacter => {
                formatter.write_str("component variant key содержит control character")
            }
            Self::TooLong {
                provided_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "component variant key занимает {provided_bytes} UTF-8 байт при лимите {maximum_bytes}"
            ),
        }
    }
}

impl std::error::Error for ComponentVariantKeyError {}

/// Opaque snapshot-local key component variant.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentVariantExactKey(String);

impl ComponentVariantExactKey {
    /// Проверяет key, сохраняя его exact UTF-8 bytes только внутри opaque value.
    pub fn new(exact_key: impl Into<String>) -> Result<Self, ComponentVariantKeyError> {
        let exact_key = exact_key.into();
        validate_component_variant_key(&exact_key)?;
        Ok(Self(exact_key))
    }
}

impl fmt::Debug for ComponentVariantExactKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentVariantExactKey")
            .field("utf8_bytes", &self.0.len())
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Opaque refresh-stable semantic key component variant.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentVariantSemanticKey(String);

impl ComponentVariantSemanticKey {
    /// Проверяет key, не публикуя raw строковый accessor.
    pub fn new(semantic_key: impl Into<String>) -> Result<Self, ComponentVariantKeyError> {
        let semantic_key = semantic_key.into();
        validate_component_variant_key(&semantic_key)?;
        Ok(Self(semantic_key))
    }
}

impl fmt::Debug for ComponentVariantSemanticKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentVariantSemanticKey")
            .field("utf8_bytes", &self.0.len())
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Проверяет общий key contract без service-specific normalization.
fn validate_component_variant_key(exact_key: &str) -> Result<(), ComponentVariantKeyError> {
    if exact_key.is_empty() {
        return Err(ComponentVariantKeyError::Empty);
    }
    if exact_key.chars().any(char::is_control) {
        return Err(ComponentVariantKeyError::ContainsControlCharacter);
    }
    if exact_key.len() > MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES {
        return Err(ComponentVariantKeyError::TooLong {
            provided_bytes: exact_key.len(),
            maximum_bytes: MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES,
        });
    }
    Ok(())
}

/// Собственная generation immutable component catalog.
///
/// Она намеренно не переиспользует `ExtractionGeneration`: refresh catalog и
/// re-extraction parent candidate имеют разные lifecycle owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentVariantCatalogGeneration(u64);

impl ComponentVariantCatalogGeneration {
    /// Создаёт generation из authority-owned монотонного значения.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает opaque numeric value для process-local correlation.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Identity конкретного catalog, привязанного к active parent candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentVariantCatalogIdentity {
    /// Exact+semantic identity active parent candidate.
    parent: ExactSelectionIdentity,
    /// Отдельная generation component catalog.
    generation: ComponentVariantCatalogGeneration,
}

impl ComponentVariantCatalogIdentity {
    /// Создаёт identity catalog без изменения parent selection semantics.
    pub const fn new(
        parent: ExactSelectionIdentity,
        generation: ComponentVariantCatalogGeneration,
    ) -> Self {
        Self { parent, generation }
    }

    /// Возвращает active parent candidate identity.
    pub const fn parent(&self) -> &ExactSelectionIdentity {
        &self.parent
    }

    /// Возвращает catalog generation.
    pub const fn generation(&self) -> ComponentVariantCatalogGeneration {
        self.generation
    }

    /// Возвращает source lineage active parent candidate.
    pub const fn source(&self) -> SourceIdentity {
        self.parent.exact().source()
    }
}

/// Snapshot-local exact identity одного component variant.
#[derive(Clone, PartialEq, Eq)]
pub struct ComponentVariantExactIdentity {
    /// Catalog scope включает active parent и component generation.
    catalog: ComponentVariantCatalogIdentity,
    /// Axis является частью identity и не выводится из callsite.
    component: ComponentKind,
    /// Opaque exact key внутри catalog.
    key: ComponentVariantExactKey,
}

impl ComponentVariantExactIdentity {
    /// Создаёт exact identity с явной component axis.
    pub const fn new(
        catalog: ComponentVariantCatalogIdentity,
        component: ComponentKind,
        key: ComponentVariantExactKey,
    ) -> Self {
        Self {
            catalog,
            component,
            key,
        }
    }

    /// Возвращает catalog scope.
    pub const fn catalog(&self) -> &ComponentVariantCatalogIdentity {
        &self.catalog
    }

    /// Возвращает component axis.
    pub const fn component(&self) -> ComponentKind {
        self.component
    }
}

impl fmt::Debug for ComponentVariantExactIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentVariantExactIdentity")
            .field("catalog", &self.catalog)
            .field("component", &self.component)
            .field("key", &self.key)
            .finish()
    }
}

/// Refresh-stable semantic identity одного component variant.
///
/// Snapshot-local parent identity и catalog generation здесь отсутствуют
/// намеренно. Refresh-stable parent candidate identity и axis остаются частью
/// identity, поэтому semantic rematch не пересекает parent lineage/axis.
#[derive(Clone, PartialEq, Eq)]
pub struct ComponentVariantSemanticIdentity {
    /// Refresh-stable parent ограничивает semantic match одной candidate lineage.
    parent: SemanticIdentity,
    /// Axis является частью semantic identity.
    component: ComponentKind,
    /// Opaque refresh-stable key.
    key: ComponentVariantSemanticKey,
}

impl ComponentVariantSemanticIdentity {
    /// Создаёт semantic identity с явной component axis.
    pub const fn new(
        parent: SemanticIdentity,
        component: ComponentKind,
        key: ComponentVariantSemanticKey,
    ) -> Self {
        Self {
            parent,
            component,
            key,
        }
    }

    /// Возвращает refresh-stable parent candidate identity.
    pub const fn parent(&self) -> &SemanticIdentity {
        &self.parent
    }

    /// Возвращает source lineage active parent candidate.
    pub const fn source(&self) -> SourceIdentity {
        self.parent.source()
    }

    /// Возвращает component axis.
    pub const fn component(&self) -> ComponentKind {
        self.component
    }
}

impl fmt::Debug for ComponentVariantSemanticIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentVariantSemanticIdentity")
            .field("parent", &self.parent)
            .field("component", &self.component)
            .field("key", &self.key)
            .finish()
    }
}

/// Video variant с neutral track descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoComponentVariant {
    /// Snapshot-local identity.
    exact_identity: ComponentVariantExactIdentity,
    /// Refresh-stable identity.
    semantic_identity: ComponentVariantSemanticIdentity,
    /// Existing normalized video track metadata.
    track: VideoTrackDescriptor,
}

impl VideoComponentVariant {
    /// Собирает provider-normalized row; полный scope/axis validation делает catalog.
    pub const fn new(
        exact_identity: ComponentVariantExactIdentity,
        semantic_identity: ComponentVariantSemanticIdentity,
        track: VideoTrackDescriptor,
    ) -> Self {
        Self {
            exact_identity,
            semantic_identity,
            track,
        }
    }

    /// Возвращает snapshot-local identity.
    pub const fn exact_identity(&self) -> &ComponentVariantExactIdentity {
        &self.exact_identity
    }

    /// Возвращает refresh-stable identity.
    pub const fn semantic_identity(&self) -> &ComponentVariantSemanticIdentity {
        &self.semantic_identity
    }

    /// Возвращает neutral video track descriptor.
    pub const fn track(&self) -> &VideoTrackDescriptor {
        &self.track
    }
}

/// Audio variant с neutral track descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioComponentVariant {
    /// Snapshot-local identity.
    exact_identity: ComponentVariantExactIdentity,
    /// Refresh-stable identity.
    semantic_identity: ComponentVariantSemanticIdentity,
    /// Existing normalized audio track metadata.
    track: AudioTrackDescriptor,
}

impl AudioComponentVariant {
    /// Собирает provider-normalized row; полный scope/axis validation делает catalog.
    pub const fn new(
        exact_identity: ComponentVariantExactIdentity,
        semantic_identity: ComponentVariantSemanticIdentity,
        track: AudioTrackDescriptor,
    ) -> Self {
        Self {
            exact_identity,
            semantic_identity,
            track,
        }
    }

    /// Возвращает snapshot-local identity.
    pub const fn exact_identity(&self) -> &ComponentVariantExactIdentity {
        &self.exact_identity
    }

    /// Возвращает refresh-stable identity.
    pub const fn semantic_identity(&self) -> &ComponentVariantSemanticIdentity {
        &self.semantic_identity
    }

    /// Возвращает neutral audio track descriptor.
    pub const fn track(&self) -> &AudioTrackDescriptor {
        &self.track
    }
}

/// Ошибка explicit catalog limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentVariantCatalogLimitError {
    /// Нулевой limit не может принять ни один поддерживаемый layout.
    Zero,
    /// Caller limit превышает crate safety ceiling.
    AboveMaximum {
        /// Запрошенное число variants.
        provided_entries: usize,
        /// Общий safety ceiling.
        maximum_entries: usize,
    },
}

impl fmt::Display for ComponentVariantCatalogLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("component variant catalog limit равен нулю"),
            Self::AboveMaximum {
                provided_entries,
                maximum_entries,
            } => write!(
                formatter,
                "component variant catalog limit {provided_entries} превышает максимум {maximum_entries}"
            ),
        }
    }
}

impl std::error::Error for ComponentVariantCatalogLimitError {}

/// Intent-named checked budget одного component catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentVariantCatalogLimit(NonZeroUsize);

impl ComponentVariantCatalogLimit {
    /// Проверяет ненулевой caller budget и общий safety ceiling.
    pub fn new(maximum_entries: usize) -> Result<Self, ComponentVariantCatalogLimitError> {
        let maximum_entries =
            NonZeroUsize::new(maximum_entries).ok_or(ComponentVariantCatalogLimitError::Zero)?;
        if maximum_entries.get() > MAX_COMPONENT_VARIANT_CATALOG_ENTRIES {
            return Err(ComponentVariantCatalogLimitError::AboveMaximum {
                provided_entries: maximum_entries.get(),
                maximum_entries: MAX_COMPONENT_VARIANT_CATALOG_ENTRIES,
            });
        }
        Ok(Self(maximum_entries))
    }

    /// Возвращает максимальную суммарную cardinality `V + A`.
    pub const fn maximum_entries(self) -> usize {
        self.0.get()
    }
}

/// Входная shape catalog до проверки и превращения collections в immutable slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentVariantCatalogEntries {
    /// Независимые video и audio axes.
    VideoAndAudio {
        /// Video variants без Cartesian multiplication.
        video: Vec<VideoComponentVariant>,
        /// Audio variants без Cartesian multiplication.
        audio: Vec<AudioComponentVariant>,
    },
    /// Только video axis.
    VideoOnly {
        /// Video variants.
        video: Vec<VideoComponentVariant>,
    },
    /// Только audio axis.
    AudioOnly {
        /// Audio variants.
        audio: Vec<AudioComponentVariant>,
    },
}

/// Полностью проверенный immutable component catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentVariantCatalog {
    /// Независимые required video и audio axes.
    ///
    /// Variant нельзя собрать вне crate в обход `ComponentVariantCatalog::new`.
    #[non_exhaustive]
    VideoAndAudio {
        /// Catalog scope.
        identity: ComponentVariantCatalogIdentity,
        /// Immutable video variants.
        video: Box<[VideoComponentVariant]>,
        /// Immutable audio variants.
        audio: Box<[AudioComponentVariant]>,
    },
    /// Required video axis без audio axis.
    ///
    /// Variant нельзя собрать вне crate в обход `ComponentVariantCatalog::new`.
    #[non_exhaustive]
    VideoOnly {
        /// Catalog scope.
        identity: ComponentVariantCatalogIdentity,
        /// Immutable video variants.
        video: Box<[VideoComponentVariant]>,
    },
    /// Required audio axis без video axis.
    ///
    /// Variant нельзя собрать вне crate в обход `ComponentVariantCatalog::new`.
    #[non_exhaustive]
    AudioOnly {
        /// Catalog scope.
        identity: ComponentVariantCatalogIdentity,
        /// Immutable audio variants.
        audio: Box<[AudioComponentVariant]>,
    },
}

/// Exact selection request с layout shape, исключающей ambiguous `Option`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentVariantSelectionRequest {
    /// Ровно один video и один audio variant.
    VideoAndAudio {
        /// Exact video identity.
        video: ComponentVariantExactIdentity,
        /// Exact audio identity.
        audio: ComponentVariantExactIdentity,
    },
    /// Ровно один video variant.
    VideoOnly {
        /// Exact video identity.
        video: ComponentVariantExactIdentity,
    },
    /// Ровно один audio variant.
    AudioOnly {
        /// Exact audio identity.
        audio: ComponentVariantExactIdentity,
    },
}

/// Проверенная immutable selection, содержащая ровно required axes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentVariantSelection {
    /// Выбранные независимые video и audio variants.
    ///
    /// Variant создаётся только validated catalog lookup-ом.
    #[non_exhaustive]
    VideoAndAudio {
        /// Exact selected video row.
        video: Box<VideoComponentVariant>,
        /// Exact selected audio row.
        audio: Box<AudioComponentVariant>,
    },
    /// Выбранный video variant.
    ///
    /// Variant создаётся только validated catalog lookup-ом.
    #[non_exhaustive]
    VideoOnly {
        /// Exact selected video row.
        video: Box<VideoComponentVariant>,
    },
    /// Выбранный audio variant.
    ///
    /// Variant создаётся только validated catalog lookup-ом.
    #[non_exhaustive]
    AudioOnly {
        /// Exact selected audio row.
        audio: Box<AudioComponentVariant>,
    },
}

impl ComponentVariantSelection {
    /// Создаёт layout-shaped exact request из канонической immutable selection.
    ///
    /// Метод клонирует exact identities выбранных rows, не меняя selection и
    /// не подменяя catalog generation или parent identity.
    pub fn exact_selection_request(&self) -> ComponentVariantSelectionRequest {
        match self {
            Self::VideoAndAudio { video, audio } => {
                ComponentVariantSelectionRequest::VideoAndAudio {
                    video: video.exact_identity().clone(),
                    audio: audio.exact_identity().clone(),
                }
            }
            Self::VideoOnly { video } => ComponentVariantSelectionRequest::VideoOnly {
                video: video.exact_identity().clone(),
            },
            Self::AudioOnly { audio } => ComponentVariantSelectionRequest::AudioOnly {
                audio: audio.exact_identity().clone(),
            },
        }
    }
}

/// Ошибки catalog admission, lookup и immutable replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentVariantError {
    /// Variant или request принадлежит другому source lineage.
    SourceMismatch,
    /// Variant или request принадлежит другому active parent candidate.
    CrossParent,
    /// Exact identity относится к другой component catalog generation.
    StaleCatalogGeneration {
        /// Generation текущего catalog.
        expected: ComponentVariantCatalogGeneration,
        /// Generation request/variant.
        provided: ComponentVariantCatalogGeneration,
    },
    /// Identity помещена не в свою axis.
    WrongAxis {
        /// Axis, которую требует операция.
        expected: ComponentKind,
        /// Axis из identity.
        provided: ComponentKind,
    },
    /// Exact identity отсутствует в текущем catalog.
    MissingVariant {
        /// Axis, в которой выполнялся lookup.
        component: ComponentKind,
    },
    /// Refresh-stable identity отсутствует в свежем catalog.
    MissingSemanticVariant {
        /// Axis, в которой выполнялся semantic lookup.
        component: ComponentKind,
    },
    /// Catalog содержит повторяющуюся snapshot-local identity.
    DuplicateExactIdentity {
        /// Axis duplicate identity.
        component: ComponentKind,
    },
    /// Две rows имеют одну refresh-stable identity, поэтому rematch неоднозначен.
    AmbiguousSemanticIdentity {
        /// Axis ambiguous identity.
        component: ComponentKind,
    },
    /// Суммарная cardinality `V + A` превышает explicit caller budget.
    CatalogLimitExceeded {
        /// Фактическое число rows.
        provided_entries: usize,
        /// Caller-owned checked limit.
        maximum_entries: usize,
    },
    /// Required axis пуста или отсутствует в layout.
    MissingRequiredAxis {
        /// Required axis.
        component: ComponentKind,
    },
    /// Request/selection shape не совпадает с catalog layout.
    LayoutMismatch,
}

impl fmt::Display for ComponentVariantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceMismatch => {
                formatter.write_str("component variant принадлежит другому source")
            }
            Self::CrossParent => {
                formatter.write_str("component variant принадлежит другому parent candidate")
            }
            Self::StaleCatalogGeneration { expected, provided } => write!(
                formatter,
                "component catalog generation устарела: ожидалась {}, получена {}",
                expected.value(),
                provided.value()
            ),
            Self::WrongAxis { expected, provided } => write!(
                formatter,
                "component variant axis не совпадает: ожидалась {expected:?}, получена {provided:?}"
            ),
            Self::MissingVariant { component } => {
                write!(
                    formatter,
                    "exact {component:?} variant отсутствует в catalog"
                )
            }
            Self::MissingSemanticVariant { component } => write!(
                formatter,
                "semantic {component:?} variant отсутствует в catalog"
            ),
            Self::DuplicateExactIdentity { component } => {
                write!(
                    formatter,
                    "catalog содержит duplicate exact {component:?} identity"
                )
            }
            Self::AmbiguousSemanticIdentity { component } => write!(
                formatter,
                "catalog содержит ambiguous semantic {component:?} identity"
            ),
            Self::CatalogLimitExceeded {
                provided_entries,
                maximum_entries,
            } => write!(
                formatter,
                "catalog содержит {provided_entries} rows при лимите {maximum_entries}"
            ),
            Self::MissingRequiredAxis { component } => {
                write!(
                    formatter,
                    "required {component:?} axis пуста или отсутствует"
                )
            }
            Self::LayoutMismatch => {
                formatter.write_str("component selection shape не совпадает с catalog layout")
            }
        }
    }
}

impl std::error::Error for ComponentVariantError {}

#[path = "variant/catalog_impl.rs"]
mod catalog_impl;

#[path = "variant/semantic_rematch.rs"]
mod semantic_rematch;

pub use semantic_rematch::ComponentVariantSemanticSelectionRequest;

#[cfg(test)]
#[path = "variant/test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "variant/tests_catalog.rs"]
mod tests_catalog;

#[cfg(test)]
#[path = "variant/tests_identity.rs"]
mod tests_identity;

#[cfg(test)]
#[path = "variant/tests_semantic_rematch.rs"]
mod tests_semantic_rematch;
