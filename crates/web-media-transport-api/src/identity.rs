//! Exact provider/component/runtime generation identities.

use std::fmt;

use web_media_core::{CandidateIdentity, SemanticIdentity};

/// Максимальный размер canonical provider ID.
const MAX_PROVIDER_ID_BYTES: usize = 64;

/// Process-local canonical identity concrete transport provider-а.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransportProviderId(String);

impl TransportProviderId {
    /// Проверяет bounded lowercase ASCII identity.
    pub fn new(value: impl Into<String>) -> Result<Self, TransportProviderIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(TransportProviderIdError::Empty);
        }
        if value.len() > MAX_PROVIDER_ID_BYTES {
            return Err(TransportProviderIdError::TooLong);
        }
        let mut characters = value.chars();
        let first = characters.next().expect("non-empty provider identity");
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(TransportProviderIdError::InvalidGrammar);
        }
        if !characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        }) {
            return Err(TransportProviderIdError::InvalidGrammar);
        }
        Ok(Self(value))
    }

    /// Возвращает canonical safe identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TransportProviderId {
    /// Provider ID не содержит request material и безопасен для diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("TransportProviderId")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for TransportProviderId {
    /// Показывает canonical provider ID.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Ошибка provider identity без отражения исходного untrusted текста.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportProviderIdError {
    /// Identity не может быть пустой.
    #[error("transport provider identity пуста")]
    Empty,
    /// Identity превышает фиксированный API budget.
    #[error("transport provider identity превышает допустимую длину")]
    TooLong,
    /// Identity нарушает lowercase ASCII grammar.
    #[error("transport provider identity имеет некорректный формат")]
    InvalidGrammar,
}

/// Семантическая роль одного реального request component-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MediaComponentRole {
    /// Один resource содержит audio и video.
    Muxed,
    /// Resource содержит только video.
    Video,
    /// Resource содержит только audio.
    Audio,
    /// Resource содержит subtitle/caption payload.
    Subtitle,
}

/// Exact snapshot identity + refresh-stable semantic identity component-а.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaComponentIdentity {
    /// Snapshot-local exact candidate identity.
    exact: CandidateIdentity,
    /// Stable rematch identity внутри той же source lineage.
    semantic: SemanticIdentity,
    /// Role различает реальные resources compound candidate-а.
    role: MediaComponentRole,
}

impl MediaComponentIdentity {
    /// Создаёт component identity только внутри одной source lineage.
    pub fn new(
        exact: CandidateIdentity,
        semantic: SemanticIdentity,
        role: MediaComponentRole,
    ) -> Result<Self, MediaComponentIdentityError> {
        if exact.source() != semantic.source() {
            return Err(MediaComponentIdentityError::SourceMismatch);
        }
        Ok(Self {
            exact,
            semantic,
            role,
        })
    }

    /// Возвращает exact snapshot identity.
    #[must_use]
    pub const fn exact(&self) -> &CandidateIdentity {
        &self.exact
    }

    /// Возвращает refresh-stable semantic identity.
    #[must_use]
    pub const fn semantic(&self) -> &SemanticIdentity {
        &self.semantic
    }

    /// Возвращает semantic resource role.
    #[must_use]
    pub const fn role(&self) -> MediaComponentRole {
        self.role
    }
}

impl fmt::Debug for MediaComponentIdentity {
    /// Делегирует safe formatting neutral opaque identities.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaComponentIdentity")
            .field("exact", &self.exact)
            .field("semantic", &self.semantic)
            .field("role", &self.role)
            .finish()
    }
}

/// Ошибка нарушения identity lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MediaComponentIdentityError {
    /// Exact и semantic identities принадлежат разным sources.
    #[error("exact и semantic component identities принадлежат разным sources")]
    SourceMismatch,
}

/// Runtime generation concrete opened source-а, независимая от extraction generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceGeneration(u64);

impl SourceGeneration {
    /// Создаёт explicit runtime generation.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает raw generation только для exact fence comparison.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Создаёт следующую generation с checked overflow.
    pub const fn next(self) -> Result<Self, SourceGenerationError> {
        match self.0.checked_add(1) {
            Some(value) => Ok(Self(value)),
            None => Err(SourceGenerationError::Exhausted),
        }
    }
}

/// Ошибка runtime generation allocator-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceGenerationError {
    /// `u64` generation space исчерпан.
    #[error("source generation space исчерпан")]
    Exhausted,
}
