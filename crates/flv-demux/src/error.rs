/// Ошибка конфигурации bounded parser policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlvOptionsError {
    /// Нулевой limit отключил бы safety boundary.
    #[error("FLV limit `{name}` должен быть больше нуля")]
    ZeroLimit { name: &'static str },
}

/// Typed container/config/lifecycle failure без раскрытия media payload.
#[derive(Debug, thiserror::Error)]
pub enum FlvDemuxError {
    /// Cooperative cancellation подтверждён token-ом.
    #[error("FLV операция отменена")]
    Cancelled,
    /// Probe/open получил не FLV/F4F input shape.
    #[error("input shape `{input}` не поддерживается container-ом `{container}`")]
    UnsupportedInput {
        container: &'static str,
        input: &'static str,
    },
    /// Raw FLV signature/header повреждены.
    #[error("повреждённый FLV header: {reason}")]
    InvalidHeader { reason: String },
    /// Tag framing либо обязательные reserved fields повреждены.
    #[error("повреждённый FLV tag на offset {offset}: {reason}")]
    MalformedTag { offset: u64, reason: String },
    /// Payload превысил явную memory boundary.
    #[error("FLV tag на offset {offset} объявляет {declared_bytes} bytes при limit {limit_bytes}")]
    TagTooLarge {
        offset: u64,
        declared_bytes: usize,
        limit_bytes: usize,
    },
    /// Codec/tag identity известна, но намеренно не входит в selected S30 profile.
    #[error("FLV codec/tag semantics не поддерживаются: {reason}")]
    UnsupportedCodec { reason: String },
    /// Sequence/config bytes не прошли codec-core validation.
    #[error("некорректная FLV sequence configuration для {codec}: {reason}")]
    InvalidConfiguration { codec: &'static str, reason: String },
    /// До packet emission не найден обязательный playable track/config.
    #[error("в bounded FLV discovery не найден playable track")]
    NoPlayableTrack,
    /// Source read/seek failure.
    #[error("FLV source failure: {reason}")]
    Source { reason: String },
    /// F4F envelope либо box topology повреждены.
    #[error("повреждённый F4F segment {sequence}: {reason}")]
    MalformedF4f { sequence: u64, reason: String },
    /// Ordered source нарушил exact monotonic sequence.
    #[error("F4F sequence mismatch: ожидался {expected}, получен {actual}")]
    SegmentSequence { expected: u64, actual: u64 },
    /// Fragment превысил bounded memory/work limit.
    #[error("F4F fragment {sequence} имеет {actual_bytes} bytes при limit {limit_bytes}")]
    FragmentTooLarge {
        sequence: u64,
        actual_bytes: usize,
        limit_bytes: usize,
    },
    /// Seek невозможен для текущего input.
    #[error("FLV seek недоступен для текущего input")]
    NotSeekable,
    /// Metadata/index anchor не удалось доказать actual tag scan-ом.
    #[error("bounded FLV index не нашёл config-safe keyframe anchor до {target:?}")]
    SeekAnchorUnavailable { target: std::time::Duration },
    /// Seek scan исчерпал named tag budget до EOS либо anchor, покрывающего target.
    #[error("FLV seek scan исчерпал limit {scanned_tags} tags без EOS или anchor для {target:?}")]
    SeekScanBudgetExhausted {
        /// Запрошенная позиция.
        target: std::time::Duration,
        /// Число проверенных tags.
        scanned_tags: usize,
    },
    /// Bounded recovery исчерпана без доказанного tag boundary.
    #[error("FLV framing recovery не нашёл boundary в пределах {searched_bytes} bytes")]
    RecoveryExhausted { searched_bytes: usize },
    /// Post-resync config/keyframe gate исчерпал отдельный cumulative byte budget.
    #[error(
        "FLV recovery gate исчерпал {limit_bytes}-byte budget после {processed_bytes} bytes; следующий tag требует {next_tag_bytes} bytes"
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
