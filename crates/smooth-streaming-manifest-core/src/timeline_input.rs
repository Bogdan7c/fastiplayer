//! Parser-only raw timeline vocabulary до compact normalization.

/// Raw `t` intent без ambiguous numeric sentinel-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmoothChunkStart {
    Explicit(u64),
    Inferred,
}

/// Raw `d` intent; inference разрешена только от следующего explicit `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmoothChunkDuration {
    Explicit(u64),
    InferFromNextExplicitStart,
}

/// Raw `r` intent использует весь positive `u64` domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmoothChunkRepeat {
    ImplicitSingle,
    Declared(u64),
}

/// Один parser-facing chunk entry до normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SmoothChunkEntry {
    pub(crate) start: SmoothChunkStart,
    pub(crate) duration: SmoothChunkDuration,
    pub(crate) repeat: SmoothChunkRepeat,
}

impl SmoothChunkEntry {
    /// Constructor остаётся внутри crate и не даёт downstream обойти parser.
    #[must_use]
    pub(crate) const fn new(
        start: SmoothChunkStart,
        duration: SmoothChunkDuration,
        repeat: SmoothChunkRepeat,
    ) -> Self {
        Self {
            start,
            duration,
            repeat,
        }
    }
}

/// Declared fragment count не смешивается с отсутствующим optional attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmoothDeclaredFragmentCount {
    #[cfg(test)]
    Unspecified,
    Exact(u64),
}
