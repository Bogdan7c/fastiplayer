/// Ошибка конфигурации bounded parser policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MpegTsOptionsError {
    /// Нулевая граница отключила бы защиту от unbounded work.
    #[error("MPEG-TS limit `{name}` должен быть больше нуля")]
    ZeroLimit {
        /// Имя ошибочного policy field.
        name: &'static str,
    },
}

/// Typed MPEG-TS parse/lifecycle failure.
#[derive(Debug, thiserror::Error)]
pub enum MpegTsDemuxError {
    /// Cooperative cancellation подтверждён shared token-ом.
    #[error("MPEG-TS операция отменена")]
    Cancelled,
    /// 192-byte M2TS намеренно не входит в доказанный профиль.
    #[error("192-byte M2TS framing не поддерживается; требуется 188-byte MPEG-TS")]
    UnsupportedM2ts,
    /// Ограниченный resync не нашёл устойчивую 188-byte синхронизацию.
    #[error("MPEG-TS sync потерян: просмотрено не больше {searched_bytes} bytes")]
    SyncLost {
        /// Фактический configured search bound.
        searched_bytes: usize,
    },
    /// PSI/PES структура повреждена.
    #[error("повреждённый MPEG-TS: {reason}")]
    Malformed {
        /// Secret-safe причина без media payload.
        reason: String,
    },
    /// Несколько программ можно воспроизвести, а silent selection запрещён.
    #[error("MPEG-TS содержит несколько playable programs: {programs:?}")]
    MultiplePlayablePrograms {
        /// Program numbers, которые имеют поддерживаемые A/V streams.
        programs: Vec<u16>,
    },
    /// В bounded initial window не найден ни один поддерживаемый program.
    #[error("в bounded MPEG-TS probe не найден playable PAT/PMT program")]
    NoPlayableProgram,
    /// Scrambled payload нельзя безопасно передать decoder-у.
    #[error("scrambled MPEG-TS payload на PID {pid}")]
    Scrambled {
        /// PID с transport_scrambling_control != 0.
        pid: u16,
    },
    /// PES превысил явную memory boundary.
    #[error("PES на PID {pid} превысил limit {limit_bytes} bytes")]
    PesTooLarge {
        /// Elementary PID.
        pid: u16,
        /// Configured bound.
        limit_bytes: usize,
    },
    /// Stateful video AU assembly превысил отдельную memory boundary.
    #[error("video access unit на PID {pid} превысил limit {limit_bytes} bytes")]
    VideoAccessUnitTooLarge {
        /// Elementary video PID.
        pid: u16,
        /// Configured bound.
        limit_bytes: usize,
    },
    /// Source/segment read не удалось.
    #[error("MPEG-TS source read failure: {reason}")]
    Source {
        /// Bounded adapter diagnostic.
        reason: String,
    },
    /// Источник не поддерживает seek.
    #[error("MPEG-TS seek недоступен для текущего input")]
    NotSeekable,
    /// Sparse index не содержит decode-safe anchor для target.
    #[error("bounded MPEG-TS index не нашёл decode-safe anchor до {target:?}")]
    SeekAnchorUnavailable {
        /// Исходная пользовательская позиция.
        target: std::time::Duration,
    },
}
