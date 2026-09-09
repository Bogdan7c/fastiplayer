//! Наблюдение за latest-slot отдельно от последующего lookup текстуры.
//! Не меняет решение acquire/reuse и не удерживает mutex во время subscriber callback.

use super::{LatestPresentFrameAcquire, present_frame_identity_from_lease};

/// Записывает фактический результат чтения slot-а до прежнего преобразования в Option.
/// Busy slot может привести к reuse старого кадра с Ready-текстурой: эти две
/// независимые границы нельзя объединять в один texture_lookup outcome.
pub(super) fn trace_latest_frame_acquisition(acquisition: &LatestPresentFrameAcquire) {
    if !tracing::enabled!(target: "fastiplayer::frame_cadence", tracing::Level::TRACE) {
        return;
    }

    let (handoff_lookup, identity) = match acquisition {
        LatestPresentFrameAcquire::Acquired(frame) => {
            ("acquired", Some(present_frame_identity_from_lease(frame)))
        }
        LatestPresentFrameAcquire::Empty => ("empty", None),
        LatestPresentFrameAcquire::Busy => ("busy", None),
    };

    // Результат уже снят, guard отпущен. Время события — верхняя граница
    // завершения чтения, а не точный момент lock/unlock или физического показа.
    tracing::trace!(
        target: "fastiplayer::frame_cadence",
        handoff_lookup,
        frame_pts_ns = ?identity.map(|frame| frame.pts().as_nanos()),
        render_generation = ?identity.map(|frame| frame.render_generation()),
        decoded_generation = ?identity.map(|frame| frame.decoded_generation()),
        "latest present frame acquisition completed"
    );
}
