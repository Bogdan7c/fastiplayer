//! Whole-parser rollback после typed interruption committed HLS body read-а.

use anyhow::{Context, Result};
use demux_api::OrderedResourceRestartableReadInterrupted;
use media_core::DemuxSeekCancellationToken;

use super::{
    HlsComponentDemuxer, HlsResourceAttemptObserver, SharedHlsMediaSpanIndex,
    open_epoch_with_media_span_index,
};
use crate::plan::HlsSegmentRestartCoordinate;

/// Parser либо готов читать, либо обязан целиком открыться с byte 0 exact segment-а.
pub(super) enum HlsParserReadState {
    Ready,
    RestartRequired {
        epoch_index: usize,
        restart_segment: HlsSegmentRestartCoordinate,
    },
}

impl HlsComponentDemuxer {
    /// Делает offside source authoritative только после успешной outer transaction.
    pub(crate) fn activate_committed_read(&self) -> Result<()> {
        self.current_active_read
            .activate_committed()
            .context("HLS failed to arm committed resource read")
    }

    /// Typed unwind не становится ordinary fatal и сохраняет exact rollback coordinate.
    pub(super) fn observe_current_read_error(&mut self, error: &anyhow::Error) -> Result<()> {
        if !is_restartable_read_interrupted(error) {
            return Ok(());
        }
        let restart_segment = self
            .current_active_read
            .current_restart_coordinate()
            .context("HLS interrupted resource has no exact restart coordinate")?;
        self.parser_read_state = HlsParserReadState::RestartRequired {
            epoch_index: self.current_epoch_index,
            restart_segment,
        };
        Ok(())
    }

    /// Failed replacement оставляет old component; первый следующий read лениво восстанавливает его.
    pub(super) fn restore_interrupted_current_if_needed(&mut self) -> Result<()> {
        let HlsParserReadState::RestartRequired {
            epoch_index,
            restart_segment,
        } = std::mem::replace(&mut self.parser_read_state, HlsParserReadState::Ready)
        else {
            return Ok(());
        };
        let epoch = self
            .plan
            .epochs
            .get(epoch_index)
            .and_then(|epoch| epoch.restart_tail(restart_segment))
            .ok_or_else(|| {
                anyhow::anyhow!("HLS interrupted restart отсутствует в immutable plan")
            })?;
        let media_spans = SharedHlsMediaSpanIndex::default();
        let active_read_lifecycle = self.active_read_control.new_epoch_lifecycle(&epoch);
        let reopened = open_epoch_with_media_span_index(
            self.plan.container,
            epoch,
            self.http.clone(),
            self.generation,
            self.policy,
            std::sync::Arc::clone(&self.registry),
            media_spans.clone(),
            DemuxSeekCancellationToken::new(),
            HlsResourceAttemptObserver::disabled(),
            active_read_lifecycle.clone(),
        );
        let reopened = match reopened {
            Ok(reopened) => reopened,
            Err(error) if is_restartable_read_interrupted(&error) => {
                self.parser_read_state = HlsParserReadState::RestartRequired {
                    epoch_index,
                    restart_segment,
                };
                return Err(error);
            }
            Err(error) => return Err(error.context("HLS interrupted parser rollback failed")),
        };
        active_read_lifecycle.activate_committed()?;
        let reopened_tracks = reopened.tracks().to_vec();
        self.current = reopened;
        self.media_spans = media_spans;
        self.current_active_read = active_read_lifecycle;
        self.refresh_track_mapping(&reopened_tracks)?;
        self.metadata = self
            .current
            .media_metadata()
            .or_else(|| self.metadata.clone());
        Ok(())
    }
}

fn is_restartable_read_interrupted(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<OrderedResourceRestartableReadInterrupted>())
}
