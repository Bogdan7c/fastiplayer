//! Два bounded барьера управляют только interleaving реальных потоков.
//! Worker паркуется после публикации вне handoff mutex; render отпускает его
//! во второй acquisition того же PTS и ждёт подтверждённую новую публикацию.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

const BARRIER_TIMEOUT: Duration = Duration::from_secs(3);
const WARMUP_HANDOFFS: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    pts: u128,
    render_generation: u64,
    decoded_generation: u64,
}

#[derive(Default)]
struct Fields {
    message: String,
    pts: Option<u128>,
    render_generation: Option<u64>,
    decoded_generation: Option<u64>,
    texture_lookup: Option<String>,
    handoff_lookup: Option<String>,
    acquired: bool,
}

impl Fields {
    fn identity(&self) -> Option<Identity> {
        Some(Identity {
            pts: self.pts?,
            render_generation: self.render_generation?,
            decoded_generation: self.decoded_generation?,
        })
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let formatted = format!("{value:?}");
        let numeric = formatted
            .strip_prefix("Some(")
            .and_then(|v| v.strip_suffix(')'))
            .unwrap_or(&formatted);
        match field.name() {
            "message" => self.message = formatted,
            "frame_pts_ns" => self.pts = numeric.parse().ok(),
            "render_generation" => self.render_generation = numeric.parse().ok(),
            "decoded_generation" => self.decoded_generation = numeric.parse().ok(),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "texture_lookup" => self.texture_lookup = Some(value.to_owned()),
            "handoff_lookup" => self.handoff_lookup = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "acquired" {
            self.acquired = value;
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "frame_pts_ns" => self.pts = Some(u128::from(value)),
            "render_generation" => self.render_generation = Some(value),
            "decoded_generation" => self.decoded_generation = Some(value),
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
struct GateState {
    handoffs: usize,
    prepared: Option<Identity>,
    prepared_lookup: Option<String>,
    slot_lookup: Option<String>,
    slot_identity: Option<Identity>,
    pinned: Option<Identity>,
    pinned_presented: bool,
    worker_released: bool,
    required: Option<Identity>,
    armed: bool,
    acquired: bool,
    rendered: Option<Identity>,
    failure: Option<&'static str>,
}

/// Меняется только положение публикации относительно production preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PublicationWindow {
    BeforePreparation,
    DuringAcquisition,
}

#[derive(Clone)]
pub(super) struct CadenceGate(Arc<(Mutex<GateState>, Condvar)>, PublicationWindow);

impl CadenceGate {
    pub(super) fn new(window: PublicationWindow) -> Self {
        Self(Arc::default(), window)
    }

    fn wait_for_next_publication<'a>(
        &self,
        mut state: MutexGuard<'a, GateState>,
    ) -> MutexGuard<'a, GateState> {
        state.worker_released = true;
        self.0.1.notify_all();
        let (mut resumed, timeout) = self
            .0
            .1
            .wait_timeout_while(state, BARRIER_TIMEOUT, |state| {
                state.required.is_none() && state.failure.is_none()
            })
            .expect("next publication barrier");
        if timeout.timed_out() && resumed.required.is_none() {
            resumed.failure = Some("worker did not publish another frame before barrier timeout");
        }
        resumed.armed = resumed.failure.is_none();
        resumed
    }

    pub(super) fn abort(&self, reason: &'static str) {
        let mut state = self.0.0.lock().expect("cadence gate");
        state.failure.get_or_insert(reason);
        state.worker_released = true;
        self.0.1.notify_all();
    }

    pub(super) fn release_worker(&self) {
        self.0.0.lock().expect("cadence gate").worker_released = true;
        self.0.1.notify_all();
    }

    pub(super) fn finished(&self) -> bool {
        let state = self.0.0.lock().expect("cadence gate");
        state.rendered.is_some() || state.failure.is_some()
    }

    pub(super) fn assert_fresh_handoff(&self) {
        let state = self.0.0.lock().expect("cadence gate");
        eprintln!("CADENCE_PRODUCTION_RECEIPT window={:?} {state:?}", self.1);
        assert_eq!(
            state.failure, None,
            "environment or barrier failure, not a cadence repro"
        );
        assert!(state.acquired, "must complete actual surface acquisition");
        let pinned = state.pinned.expect("first frame actually handed off");
        let required = state
            .required
            .expect("new frame published inside acquisition interval");
        let rendered = state.rendered.expect("actual Presented-gated handoff");
        assert_eq!(
            state.slot_lookup.as_deref(),
            Some("acquired"),
            "Ready texture must not hide Busy-slot reuse in this causal check"
        );
        assert_eq!(
            state.slot_identity,
            Some(rendered),
            "actual acquired lease must reach the surface handoff"
        );
        assert_eq!(
            state.prepared_lookup.as_deref(),
            Some("ready"),
            "target handoff must use Ready input"
        );
        assert_eq!(
            state.prepared,
            Some(rendered),
            "handoff must identify the actual prepared input"
        );
        assert!(required.pts > pinned.pts);
        assert_eq!(required.render_generation, rendered.render_generation);
        assert_eq!(required.decoded_generation, rendered.decoded_generation);
        assert!(
            rendered.pts >= required.pts,
            "CADENCE_FRESHNESS_DEFECT: published PTS {} before acquisition returned, rendered PTS {} (previous {})",
            required.pts,
            rendered.pts,
            pinned.pts
        );
    }

    fn observe(&self, fields: Fields) {
        let mut state = self.0.0.lock().expect("cadence gate");
        if state.failure.is_some() || state.rendered.is_some() {
            return;
        }
        match fields.message.as_str() {
            "latest present frame acquisition completed" => {
                state.slot_identity = fields.identity();
                state.slot_lookup = fields.handoff_lookup;
            }
            "video frame prepared for surface" => {
                state.prepared = fields.identity();
                state.prepared_lookup = fields.texture_lookup;
            }
            "latest video frame published" => {
                let Some(identity) = fields.identity() else {
                    return;
                };
                if state.pinned.is_none() && state.handoffs >= WARMUP_HANDOFFS {
                    state.pinned = Some(identity);
                    let (mut resumed, timeout) = self
                        .0
                        .1
                        .wait_timeout_while(state, BARRIER_TIMEOUT, |state| !state.worker_released)
                        .expect("worker publication barrier");
                    if timeout.timed_out() && !resumed.worker_released {
                        resumed.failure =
                            Some("render did not reach second acquisition of pinned frame");
                        resumed.worker_released = true;
                        self.0.1.notify_all();
                    }
                    return;
                }
                if state.worker_released
                    && state.required.is_none()
                    && state.pinned.is_some_and(|pinned| {
                        identity.pts > pinned.pts
                            && identity.render_generation == pinned.render_generation
                            && identity.decoded_generation == pinned.decoded_generation
                    })
                {
                    state.required = Some(identity);
                    self.0.1.notify_all();
                }
            }
            "surface acquire started"
                if self.1 == PublicationWindow::DuringAcquisition
                    && state.pinned_presented
                    && state.prepared == state.pinned =>
            {
                if state.prepared_lookup.as_deref() != Some("ready") {
                    state.failure =
                        Some("target preparation was not Ready; Busy is a separate case");
                    state.worker_released = true;
                    self.0.1.notify_all();
                    return;
                }
                drop(self.wait_for_next_publication(state));
            }
            "surface acquire finished" if state.armed => {
                state.acquired = fields.acquired;
                if !fields.acquired {
                    state.failure = Some("target surface acquisition was dropped");
                }
            }
            "current video frame submitted to surface" => {
                state.handoffs += 1;
                if state.armed {
                    state.rendered = fields.identity();
                } else if state.pinned.is_some() && fields.identity() == state.pinned {
                    state.pinned_presented = true;
                    if self.1 == PublicationWindow::BeforePreparation {
                        // Первый handoff уже состоялся, следующая preparation ещё не началась.
                        drop(self.wait_for_next_publication(state));
                    }
                }
            }
            _ => {}
        }
    }
}

impl<S: Subscriber> Layer<S> for CadenceGate {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if !matches!(
            event.metadata().target(),
            "fastiplayer::frame_cadence" | "fastiplayer::video_render_acceptance"
        ) {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.observe(fields);
    }
}
