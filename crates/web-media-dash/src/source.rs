//! Lazy bounded ordered resource source для SegmentTemplate/List/serialized paths.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use dash_mpd_core::DashMediaKind;
use demux_api::{
    OrderedSegment, OrderedSegmentDiscontinuity, OrderedSegmentKind, OrderedSegmentReadError,
    OrderedSegmentSequence, OrderedSegmentSource,
};
use source_core::SourceError;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_transport_api::SourceGeneration;

use crate::plan::DashPlannedResource;
use crate::request::DashSerializedFragmentKind;

/// Finite resource source; network выполняется только внутри demux/media-open worker-а.
pub(crate) struct DashOrderedSegmentSource {
    /// Shared S31 policy.
    http: AdaptiveHttpContext,
    /// Exact generation.
    generation: SourceGeneration,
    /// Remaining init/media resources.
    resources: VecDeque<DashPlannedResource>,
    /// Query projection semantics.
    query_application: AdaptiveResourceQueryApplication,
    /// Full-resource body bound.
    maximum_fragment_bytes: NonZeroUsize,
    /// Monotonic ordered segment sequence.
    next_sequence: u64,
    /// Optional atomic live transport owner.
    live_transport: Option<Arc<dyn DashLiveTransportProvider>>,
    /// Selected component identity для endpoint resource remap.
    live_media_kind: Option<DashMediaKind>,
    /// Global Period identity для endpoint resource remap.
    live_period_timeline_start: Option<Duration>,
}

/// Узкий live transport boundary без MPD/player/app vocabulary.
pub(crate) trait DashLiveTransportProvider: Send + Sync {
    /// Возвращает current context/generation одним atomic read.
    fn current_transport(
        &self,
    ) -> Result<(AdaptiveHttpContext, SourceGeneration), AdaptiveTransportError>;
    /// Remap-ит exact failed resource через single-flight endpoint refresh.
    fn recover_expired_resource(
        &self,
        failed_generation: SourceGeneration,
        media_kind: DashMediaKind,
        period_timeline_start: Duration,
        failed_resource: &DashPlannedResource,
    ) -> Result<DashPlannedResource, AdaptiveTransportError>;
}

impl DashOrderedSegmentSource {
    /// Создаёт source с optional seek start media index, сохраняя initialization.
    pub(crate) fn new(
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        resources: &[DashPlannedResource],
        query_application: AdaptiveResourceQueryApplication,
        maximum_fragment_bytes: NonZeroUsize,
        first_media_index: usize,
    ) -> Result<Self, OrderedSegmentReadError> {
        let initialization = resources
            .iter()
            .find(|resource| resource.kind == DashSerializedFragmentKind::Initialization)
            .cloned()
            .ok_or_else(|| OrderedSegmentReadError::Failed {
                reason: "dash-missing-initialization".to_owned(),
            })?;
        let selected_media = resources
            .iter()
            .filter(|resource| resource.kind == DashSerializedFragmentKind::Media)
            .skip(first_media_index)
            .cloned();
        let resources = std::iter::once(initialization)
            .chain(selected_media)
            .collect::<VecDeque<_>>();
        if resources.len() < 2 {
            return Err(OrderedSegmentReadError::Failed {
                reason: "dash-missing-media-fragment".to_owned(),
            });
        }
        Ok(Self {
            http,
            generation,
            resources,
            query_application,
            maximum_fragment_bytes,
            next_sequence: 0,
            live_transport: None,
            live_media_kind: None,
            live_period_timeline_start: None,
        })
    }

    /// Создаёт live source с generation-aware endpoint replacement.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_live(
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        resources: &[DashPlannedResource],
        query_application: AdaptiveResourceQueryApplication,
        maximum_fragment_bytes: NonZeroUsize,
        first_media_index: usize,
        live_transport: Arc<dyn DashLiveTransportProvider>,
        media_kind: DashMediaKind,
        period_timeline_start: Duration,
    ) -> Result<Self, OrderedSegmentReadError> {
        let mut source = Self::new(
            http,
            generation,
            resources,
            query_application,
            maximum_fragment_bytes,
            first_media_index,
        )?;
        source.live_transport = Some(live_transport);
        source.live_media_kind = Some(media_kind);
        source.live_period_timeline_start = Some(period_timeline_start);
        Ok(source)
    }

    /// Загружает один exact resource с role-specific secret scope.
    fn fetch_resource(
        &self,
        resource: &DashPlannedResource,
    ) -> Result<Vec<u8>, AdaptiveTransportError> {
        let purpose = match resource.kind {
            DashSerializedFragmentKind::Initialization => AdaptiveResourcePurpose::Initialization,
            DashSerializedFragmentKind::Media => AdaptiveResourcePurpose::MediaSegment,
        };
        let (http, generation) = match &self.live_transport {
            Some(provider) => provider.current_transport()?,
            None => (self.http.clone(), self.generation),
        };
        let request = Self::resource_request(
            resource,
            generation,
            self.maximum_fragment_bytes,
            purpose,
            self.query_application,
        );
        match http.fetch_resource_blocking(request) {
            Ok(resource) => Ok(resource.into_bytes()),
            Err(error)
                if self.live_transport.is_some()
                    && matches!(error.http_status_code(), Some(401 | 403 | 404 | 410)) =>
            {
                let provider = self.live_transport.as_ref().expect("checked live provider");
                let media_kind = self
                    .live_media_kind
                    .expect("live source always has component identity");
                let period_timeline_start = self
                    .live_period_timeline_start
                    .expect("live source always has Period identity");
                let fresh_resource = provider.recover_expired_resource(
                    generation,
                    media_kind,
                    period_timeline_start,
                    resource,
                )?;
                let (fresh_http, fresh_generation) = provider.current_transport()?;
                fresh_http
                    .fetch_resource_blocking(Self::resource_request(
                        &fresh_resource,
                        fresh_generation,
                        self.maximum_fragment_bytes,
                        purpose,
                        self.query_application,
                    ))
                    .map(web_media_adaptive::AdaptiveFetchedResource::into_bytes)
            }
            Err(error) => Err(error),
        }
    }

    /// Строит exact request для fixed либо refreshed generation.
    fn resource_request(
        resource: &DashPlannedResource,
        generation: SourceGeneration,
        maximum_fragment_bytes: NonZeroUsize,
        purpose: AdaptiveResourcePurpose,
        query_application: AdaptiveResourceQueryApplication,
    ) -> AdaptiveResourceFetchRequest {
        match resource.byte_range {
            Some(byte_range) => AdaptiveResourceFetchRequest::range(
                generation,
                resource.target.clone(),
                byte_range,
                byte_range.length(),
                purpose,
                query_application,
            ),
            None => AdaptiveResourceFetchRequest::full(
                generation,
                resource.target.clone(),
                maximum_fragment_bytes,
                purpose,
                query_application,
            ),
        }
    }
}

impl OrderedSegmentSource for DashOrderedSegmentSource {
    /// Отдаёт следующий immutable segment либо clean finite EOF.
    fn next_segment(
        &mut self,
        cancellation: &source_core::CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        if cancellation.is_cancelled() || self.http.cancellation().is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        let Some(resource) = self.resources.pop_front() else {
            return Ok(None);
        };
        let bytes = self
            .fetch_resource(&resource)
            .map_err(map_runtime_source_error)?;
        if bytes.is_empty() {
            return Err(OrderedSegmentReadError::Failed {
                reason: "dash-empty-resource".to_owned(),
            });
        }
        let kind = match resource.kind {
            DashSerializedFragmentKind::Initialization => OrderedSegmentKind::Initialization,
            DashSerializedFragmentKind::Media => OrderedSegmentKind::Media,
        };
        let sequence = OrderedSegmentSequence::new(self.next_sequence);
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(Some(OrderedSegment {
            sequence,
            kind,
            discontinuity: OrderedSegmentDiscontinuity::Continuous,
            bytes: Bytes::from(bytes),
        }))
    }
}

/// Сохраняет cancellation отдельно от bounded secret-safe failure category.
fn map_runtime_source_error(error: AdaptiveTransportError) -> OrderedSegmentReadError {
    match error {
        AdaptiveTransportError::Cancelled
        | AdaptiveTransportError::Source(SourceError::Cancelled) => {
            OrderedSegmentReadError::Cancelled
        }
        AdaptiveTransportError::Source(_)
        | AdaptiveTransportError::Target(_)
        | AdaptiveTransportError::Redirect(_)
        | AdaptiveTransportError::SecretScopeRejected
        | AdaptiveTransportError::ExplicitCookieHeader
        | AdaptiveTransportError::RestartableReadInterrupted
        | AdaptiveTransportError::WorkerStopped
        | AdaptiveTransportError::StaleGeneration { .. }
        | AdaptiveTransportError::ResourceBoundExceeded { .. }
        | AdaptiveTransportError::InvalidResourcePolicy { .. } => OrderedSegmentReadError::Failed {
            reason: "dash-resource-fetch".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DASH не вооружает restartable-read contract HLS и потому обязан fail-closed.
    #[test]
    fn restartable_read_interruption_is_an_ordinary_fatal_source_failure() {
        let error = map_runtime_source_error(AdaptiveTransportError::RestartableReadInterrupted);

        assert!(matches!(
            error,
            OrderedSegmentReadError::Failed { ref reason }
                if reason == "dash-resource-fetch"
        ));
    }
}
