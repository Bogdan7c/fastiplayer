//! Механизм, которым owner применил один route настроек.

/// Runtime mechanism used for one route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMechanism {
    /// Owner updated runtime state in-place.
    InPlace,

    /// Owner asked a worker to reconfigure itself.
    WorkerReconfigure,

    /// Owner rebuilt a pipeline while preserving its invariants.
    PipelineRebuild,

    /// Owner recreated renderer resources.
    RendererRecreate,

    /// Previously active preview became committed runtime state.
    PreviewPromoted,

    /// Value was accepted and persisted, but live runtime was intentionally left untouched:
    /// the effect begins after an application restart.
    AppliesAfterRestart,
}
