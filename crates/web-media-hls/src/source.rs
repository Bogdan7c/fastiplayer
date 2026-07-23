use std::num::NonZeroUsize;

use bytes::Bytes;
use demux_api::{
    OrderedSegment, OrderedSegmentReadError, OrderedSegmentSequence, OrderedSegmentSource,
};
use source_core::{CancellationToken, SourceError};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_transport_api::SourceGeneration;
use zeroize::Zeroizing;

use crate::plan::{HlsEpochPlan, PlannedEncryption, PlannedKeySource, PlannedResource};
use crate::{SecretAes128Key, decrypt_aes128_cbc_pkcs7};

/// Lazy finite source одного epoch; network/key/decrypt выполняются на demux worker-е.
pub(crate) struct HlsEpochSegmentSource {
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    resources: std::vec::IntoIter<PlannedResource>,
    next_sequence: u64,
    maximum_key_resource_bytes: NonZeroUsize,
    cached_key: Option<CachedKey>,
}

/// Current epoch-local key; identity не содержит URL/key bytes.
struct CachedKey {
    identity: u64,
    key: SecretAes128Key,
}

impl HlsEpochSegmentSource {
    pub(crate) fn new(
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        epoch: HlsEpochPlan,
        maximum_key_resource_bytes: NonZeroUsize,
    ) -> Self {
        Self {
            http,
            generation,
            resources: epoch.resources.into_iter(),
            next_sequence: 0,
            maximum_key_resource_bytes,
            cached_key: None,
        }
    }

    fn fetch_resource(
        &self,
        resource: &PlannedResource,
    ) -> Result<Vec<u8>, AdaptiveTransportError> {
        let purpose = match resource.kind {
            demux_api::OrderedSegmentKind::Initialization => {
                AdaptiveResourcePurpose::Initialization
            }
            demux_api::OrderedSegmentKind::Media => AdaptiveResourcePurpose::MediaSegment,
        };
        let maximum_body_bytes = self.http.maximum_resource_bytes(purpose);
        let request = match resource.byte_range {
            Some(byte_range) => AdaptiveResourceFetchRequest::range(
                self.generation,
                resource.target.clone(),
                byte_range,
                maximum_body_bytes,
                purpose,
                AdaptiveResourceQueryApplication::MergeScopedAddition,
            ),
            None => AdaptiveResourceFetchRequest::full(
                self.generation,
                resource.target.clone(),
                maximum_body_bytes,
                purpose,
                AdaptiveResourceQueryApplication::MergeScopedAddition,
            ),
        };
        self.http
            .fetch_resource_blocking(request)
            .map(web_media_adaptive::AdaptiveFetchedResource::into_bytes)
    }

    fn key_for(
        &mut self,
        encryption: &PlannedEncryption,
    ) -> Result<SecretAes128Key, HlsSegmentSourceError> {
        if let Some(cached) = self
            .cached_key
            .as_ref()
            .filter(|cached| cached.identity == encryption.key_identity)
        {
            return Ok(cached.key.clone());
        }
        let fetched_key = match &encryption.key {
            PlannedKeySource::Inline(key) => key.clone(),
            PlannedKeySource::ManifestTarget(target) => self.fetch_key_target(
                target,
                AdaptiveResourceQueryApplication::MergeScopedAddition,
            )?,
            PlannedKeySource::ExtractorReplacement(target) => {
                self.fetch_key_target(target, AdaptiveResourceQueryApplication::BypassScopedQuery)?
            }
        };
        self.cached_key = Some(CachedKey {
            identity: encryption.key_identity,
            key: fetched_key.clone(),
        });
        Ok(fetched_key)
    }

    fn fetch_key_target(
        &self,
        target: &source_core::HttpRequestTarget,
        query_application: AdaptiveResourceQueryApplication,
    ) -> Result<SecretAes128Key, HlsSegmentSourceError> {
        let fetched = self
            .http
            .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
                self.generation,
                target.clone(),
                self.maximum_key_resource_bytes,
                AdaptiveResourcePurpose::EncryptionKey,
                query_application,
            ))?;
        let key_bytes = Zeroizing::new(fetched.into_bytes());
        Ok(SecretAes128Key::from_key_file_bytes(&key_bytes)?)
    }

    fn decrypt(
        &mut self,
        ciphertext: &[u8],
        encryption: &PlannedEncryption,
    ) -> Result<Bytes, HlsSegmentSourceError> {
        let key = self.key_for(encryption)?;
        let plaintext = decrypt_aes128_cbc_pkcs7(ciphertext, &key, encryption.iv)?;
        Ok(Bytes::copy_from_slice(plaintext.expose_for_demux()))
    }
}

impl OrderedSegmentSource for HlsEpochSegmentSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        if cancellation.is_cancelled() || self.http.cancellation().is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        let Some(resource) = self.resources.next() else {
            return Ok(None);
        };
        if resource.encryption.is_none() {
            self.cached_key = None;
        }
        let fetched = self
            .fetch_resource(&resource)
            .map_err(map_runtime_source_error)?;
        let bytes = match &resource.encryption {
            Some(encryption) => self
                .decrypt(&fetched, encryption)
                .map_err(map_runtime_source_error)?,
            None => Bytes::from(fetched),
        };
        let sequence = OrderedSegmentSequence::new(self.next_sequence);
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(Some(OrderedSegment {
            sequence,
            kind: resource.kind,
            discontinuity: resource.discontinuity,
            bytes,
        }))
    }
}

#[derive(Debug, thiserror::Error)]
enum HlsSegmentSourceError {
    #[error("transport")]
    Transport(#[from] AdaptiveTransportError),
    #[error("key")]
    Key(#[from] crate::HlsKeyStateError),
    #[error("decrypt")]
    Decrypt(#[from] crate::Aes128CbcDecryptError),
}

fn map_runtime_source_error(error: impl Into<HlsSegmentSourceError>) -> OrderedSegmentReadError {
    match error.into() {
        HlsSegmentSourceError::Transport(
            AdaptiveTransportError::Cancelled
            | AdaptiveTransportError::Source(SourceError::Cancelled),
        ) => OrderedSegmentReadError::Cancelled,
        HlsSegmentSourceError::Transport(_) => OrderedSegmentReadError::Failed {
            reason: "hls-resource-fetch".to_owned(),
        },
        HlsSegmentSourceError::Key(_) => OrderedSegmentReadError::Failed {
            reason: "hls-invalid-aes-key".to_owned(),
        },
        HlsSegmentSourceError::Decrypt(_) => OrderedSegmentReadError::Failed {
            reason: "hls-invalid-aes-ciphertext".to_owned(),
        },
    }
}
