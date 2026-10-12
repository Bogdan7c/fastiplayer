/// Ошибка конфигурации bounded parser policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlvOptionsError {
    /// Нулевой limit отключил бы safety boundary.
    #[error("FLV limit '{name}' must be greater than zero")]
    ZeroLimit { name: &'static str },
}

/// Typed container/config/lifecycle failure без раскрытия media payload.
#[derive(Debug, thiserror::Error)]
pub enum FlvDemuxError {
    /// Cooperative cancellation подтверждён token-ом.
    #[error("FLV operation cancelled")]
    Cancelled,
    /// Probe/open получил не FLV/F4F input shape.
    #[error("input shape '{input}' is not supported by container '{container}'")]
    UnsupportedInput {
        container: &'static str,
        input: &'static str,
    },
    /// Raw FLV signature/header повреждены.
    #[error("damaged FLV header: {reason}")]
    InvalidHeader { reason: String },
    /// Tag framing либо обязательные reserved fields повреждены.
    #[error("damaged FLV tag on offset {offset}: {reason}")]
    MalformedTag { offset: u64, reason: String },
    /// Payload превысил явную memory boundary.
    #[error("FLV tag on offset {offset} announces {declared_bytes} bytes at limit {limit_bytes}")]
    TagTooLarge {
        offset: u64,
        declared_bytes: usize,
        limit_bytes: usize,
    },
    /// Codec/tag identity известна, но намеренно не входит в selected S30 profile.
    #[error("FLV codec/tag semantics not supported: {reason}")]
    UnsupportedCodec { reason: String },
    /// Sequence/config bytes не прошли codec-core validation.
    #[error("FLV sequence configuration for {codec}: {reason}")]
    InvalidConfiguration { codec: &'static str, reason: String },
    /// До packet emission не найден обязательный playable track/config.
    #[error("bounded FLV discovery not found playable track")]
    NoPlayableTrack,
    /// Source read/seek failure.
    #[error("FLV source failure: {reason}")]
    Source { reason: String },
    /// F4F envelope либо box topology повреждены.
    #[error("damaged F4F segment {sequence}: {reason}")]
    MalformedF4f { sequence: u64, reason: String },
    /// Ordered source нарушил exact monotonic sequence.
    #[error("F4F sequence mismatch: expected {expected}, received {actual}")]
    SegmentSequence { expected: u64, actual: u64 },
    /// Fragment превысил bounded memory/work limit.
    #[error("F4F fragment {sequence} has {actual_bytes} bytes at limit {limit_bytes}")]
    FragmentTooLarge {
        sequence: u64,
        actual_bytes: usize,
        limit_bytes: usize,
    },
    /// Seek невозможен для текущего input.
    #[error("FLV seek is not available for current input")]
    NotSeekable,
    /// Metadata/index anchor не удалось доказать actual tag scan-ом.
    #[error("bounded FLV index not found config-safe keyframe anchor to {target:?}")]
    SeekAnchorUnavailable { target: std::time::Duration },
    /// Seek scan исчерпал named tag budget до EOS либо anchor, покрывающего target.
    #[error(
        "FLV seek scan runs out of limit {scanned_tags} tags without EOS or anchor for {target:?}"
    )]
    SeekScanBudgetExhausted {
        /// Запрошенная позиция.
        target: std::time::Duration,
        /// Число проверенных tags.
        scanned_tags: usize,
    },
    /// Bounded recovery исчерпана без доказанного tag boundary.
    #[error("FLV framing recovery found no boundary within {searched_bytes} bytes")]
    RecoveryExhausted { searched_bytes: usize },
    /// Post-resync config/keyframe gate исчерпал отдельный cumulative byte budget.
    #[error(
        "FLV recovery gate has run out of {limit_bytes}-byte budget after {processed_bytes} bytes; next tag requires {next_tag_bytes} bytes"
    )]
    RecoveryGateBudgetExhausted {
        /// Уже обработанные wire bytes текущего gate-а.
        processed_bytes: usize,
        /// Wire bytes следующего целого tag-а.
        next_tag_bytes: usize,
        /// Именованный limit из options.
        limit_bytes: usize,
    },
}
