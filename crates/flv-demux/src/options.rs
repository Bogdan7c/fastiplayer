use std::num::NonZeroUsize;

use crate::FlvOptionsError;

/// Именованный ненулевой safety limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlvLimit(NonZeroUsize);

impl FlvLimit {
    /// Не позволяет вызывающему коду случайно выключить boundary нулём.
    pub fn new(value: usize, name: &'static str) -> Result<Self, FlvOptionsError> {
        NonZeroUsize::new(value)
            .map(Self)
            .ok_or(FlvOptionsError::ZeroLimit { name })
    }

    /// Возвращает проверенное значение.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

/// Все memory/work bounds FLV/F4F parser-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlvDemuxOptions {
    /// Максимальный FLV tag payload.
    pub tag_bytes: FlvLimit,
    /// Максимум tags initial track discovery.
    pub initial_tags: FlvLimit,
    /// Максимум bytes bounded framing recovery.
    pub recovery_bytes: FlvLimit,
    /// Максимум AMF nesting depth.
    pub metadata_depth: FlvLimit,
    /// Максимум AMF object entries.
    pub metadata_entries: FlvLimit,
    /// Максимум bytes одного AMF string.
    pub metadata_string_bytes: FlvLimit,
    /// Максимум retained metadata keyframe anchors.
    pub index_entries: FlvLimit,
    /// Максимум raw tags одного on-demand seek scan.
    pub seek_scan_tags: FlvLimit,
    /// Максимум bytes одного F4F fragment.
    pub fragment_bytes: FlvLimit,
    /// Максимум ISO boxes в одном F4F segment-е.
    pub fragment_boxes: FlvLimit,
}

impl Default for FlvDemuxOptions {
    fn default() -> Self {
        Self {
            tag_bytes: FlvLimit(NonZeroUsize::new(32 * 1024 * 1024).expect("non-zero")),
            initial_tags: FlvLimit(NonZeroUsize::new(4_096).expect("non-zero")),
            recovery_bytes: FlvLimit(NonZeroUsize::new(256 * 1024).expect("non-zero")),
            metadata_depth: FlvLimit(NonZeroUsize::new(16).expect("non-zero")),
            metadata_entries: FlvLimit(NonZeroUsize::new(4_096).expect("non-zero")),
            metadata_string_bytes: FlvLimit(NonZeroUsize::new(64 * 1024).expect("non-zero")),
            index_entries: FlvLimit(NonZeroUsize::new(8_192).expect("non-zero")),
            seek_scan_tags: FlvLimit(NonZeroUsize::new(32_768).expect("non-zero")),
            fragment_bytes: FlvLimit(NonZeroUsize::new(64 * 1024 * 1024).expect("non-zero")),
            fragment_boxes: FlvLimit(NonZeroUsize::new(1_024).expect("non-zero")),
        }
    }
}
