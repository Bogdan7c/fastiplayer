//! Neutral seekable/streaming transport result shape.

use std::fmt;

use source_core::{ByteSource, HttpRequestTarget, StreamingByteSource};

use crate::{MediaPresentation, OpenedComponentIdentity, SourceGeneration};

/// Typed byte seekability результата.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportSeekability {
    /// Random-access `ByteSource`.
    Seekable,
    /// Forward-only streaming reader.
    Streaming,
}

/// Owned neutral input без demux/container knowledge.
pub enum TransportInput {
    /// Seekable byte source; concrete HTTP provider переиспользует `source-core`.
    Seekable(Box<dyn ByteSource>),
    /// Forward-only reader для non-Range/live transport-а.
    Streaming(Box<dyn StreamingByteSource>),
}

impl TransportInput {
    /// Проверяет, что source действительно объявляет seekability.
    pub fn seekable(source: Box<dyn ByteSource>) -> Result<Self, TransportInputError> {
        if !source.seekability().is_seekable() {
            return Err(TransportInputError::SourceIsNotSeekable);
        }
        Ok(Self::Seekable(source))
    }

    /// Создаёт forward-only input из concrete reader-а.
    #[must_use]
    pub fn streaming(source: impl StreamingByteSource + 'static) -> Self {
        Self::Streaming(Box::new(source))
    }

    /// Возвращает typed seekability без downcast-а.
    #[must_use]
    pub const fn seekability(&self) -> TransportSeekability {
        match self {
            Self::Seekable(_) => TransportSeekability::Seekable,
            Self::Streaming(_) => TransportSeekability::Streaming,
        }
    }

    /// Передаёт seekable source следующему composition owner-у.
    pub fn into_seekable(self) -> Result<Box<dyn ByteSource>, Self> {
        match self {
            Self::Seekable(source) => Ok(source),
            streaming @ Self::Streaming(_) => Err(streaming),
        }
    }

    /// Передаёт streaming reader следующему composition owner-у.
    pub fn into_streaming(self) -> Result<Box<dyn StreamingByteSource>, Self> {
        match self {
            Self::Streaming(reader) => Ok(reader),
            seekable @ Self::Seekable(_) => Err(seekable),
        }
    }
}

impl fmt::Debug for TransportInput {
    /// Diagnostics показывает только boundary shape.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportInput")
            .field("seekability", &self.seekability())
            .finish()
    }
}

/// Ошибка provider input contract-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportInputError {
    /// Provider попытался опубликовать non-seekable source как seekable.
    #[error("transport provider вернул non-seekable source в seekable result")]
    SourceIsNotSeekable,
}

/// Успешно открытый component transport.
pub struct OpenedTransport {
    /// Caller-owned exact identity/generation.
    identity: OpenedComponentIdentity,
    /// Provider-confirmed VOD/live nature.
    presentation: MediaPresentation,
    /// Final validated target после redirect chain.
    final_target: HttpRequestTarget,
    /// Neutral byte input.
    input: TransportInput,
}

impl OpenedTransport {
    /// Собирается registry-ем после проверки provider output-а.
    pub(crate) fn new(
        identity: OpenedComponentIdentity,
        presentation: MediaPresentation,
        final_target: HttpRequestTarget,
        input: TransportInput,
    ) -> Self {
        Self {
            identity,
            presentation,
            final_target,
            input,
        }
    }

    /// Возвращает exact opened identity.
    #[must_use]
    pub const fn identity(&self) -> &OpenedComponentIdentity {
        &self.identity
    }

    /// Возвращает provider-confirmed VOD/live nature.
    #[must_use]
    pub const fn presentation(&self) -> MediaPresentation {
        self.presentation
    }

    /// Возвращает final redacted target contract.
    #[must_use]
    pub const fn final_target(&self) -> &HttpRequestTarget {
        &self.final_target
    }

    /// Возвращает typed seekability.
    #[must_use]
    pub const fn seekability(&self) -> TransportSeekability {
        self.input.seekability()
    }

    /// Передаёт input composition owner-у.
    #[must_use]
    pub fn into_input(self) -> TransportInput {
        self.input
    }
}

impl fmt::Debug for OpenedTransport {
    /// Все nested fields имеют secret-safe Debug.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenedTransport")
            .field("identity", &self.identity)
            .field("presentation", &self.presentation)
            .field("final_target", &self.final_target)
            .field("input", &self.input)
            .finish()
    }
}

/// Успешный refresh с explicit replaced generation.
pub struct RefreshedTransport {
    /// Generation, которую refresh заменил.
    replaced_generation: SourceGeneration,
    /// Новый opened transport.
    opened: OpenedTransport,
}

impl RefreshedTransport {
    /// Собирается registry-ем после exact fence validation.
    pub(crate) const fn new(
        replaced_generation: SourceGeneration,
        opened: OpenedTransport,
    ) -> Self {
        Self {
            replaced_generation,
            opened,
        }
    }

    /// Возвращает заменённую generation.
    #[must_use]
    pub const fn replaced_generation(&self) -> SourceGeneration {
        self.replaced_generation
    }

    /// Возвращает новый transport.
    #[must_use]
    pub const fn opened(&self) -> &OpenedTransport {
        &self.opened
    }

    /// Передаёт новый transport caller-у.
    #[must_use]
    pub fn into_opened(self) -> OpenedTransport {
        self.opened
    }
}

impl fmt::Debug for RefreshedTransport {
    /// Nested transport остаётся secret-safe.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RefreshedTransport")
            .field("replaced_generation", &self.replaced_generation)
            .field("opened", &self.opened)
            .finish()
    }
}
