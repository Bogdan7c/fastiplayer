use super::{PlayerWorkerRuntime, WorkerTimedWaitOutcome};

impl PlayerWorkerRuntime {
    /// Применяет latest timeline snapshot либо выключает disconnected source без busy loop.
    pub(super) fn handle_dynamic_timeline_activity(
        &mut self,
        timeline_generation: Option<media_core::DynamicMediaTimelinePortGeneration>,
        activity_result: Result<(), crossbeam_channel::RecvError>,
    ) -> WorkerTimedWaitOutcome {
        let Some(timeline_generation) = timeline_generation else {
            return WorkerTimedWaitOutcome::ContinueWaiting;
        };
        match activity_result {
            Ok(()) => {
                if self.session.refresh_dynamic_timeline() {
                    self.publish_session_outputs();
                    if let Some(wake) = self.config.timeline_activity_wake.as_ref() {
                        wake.wake_player_timeline();
                    }
                }
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: false,
                }
            }
            Err(_) => {
                self.session
                    .disconnect_dynamic_timeline_activity(timeline_generation);
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: false,
                }
            }
        }
    }
}
