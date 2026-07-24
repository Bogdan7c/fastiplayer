//! Lazy bounded ordered resource source для SegmentTemplate/List/serialized paths.

use std::collections::VecDeque;
use std::num::NonZeroUsize;

use bytes::Bytes;
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
        })
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
        let request = match resource.byte_range {
            Some(byte_range) => AdaptiveResourceFetchRequest::range(
                self.generation,
                resource.target.clone(),
                byte_range,
                byte_range.length(),
                purpose,
                self.query_application,
            ),
            None => AdaptiveResourceFetchRequest::full(
                self.generation,
                resource.target.clone(),
                self.maximum_fragment_bytes,
                purpose,
                self.query_application,
            ),
        };
        self.http
            .fetch_resource_blocking(request)
            .map(web_media_adaptive::AdaptiveFetchedResource::into_bytes)
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
        | AdaptiveTransportError::WorkerStopped
        | AdaptiveTransportError::StaleGeneration { .. }
        | AdaptiveTransportError::ResourceBoundExceeded { .. } => OrderedSegmentReadError::Failed {
            reason: "dash-resource-fetch".to_owned(),
        },
    }
}
