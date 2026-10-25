//! Подставной провайдер ресурсов декодера над настоящими буферами ядра и подставной декодер,
//! который отдаёт его под обёртку освобождения по fence (`wrap_video_backend_for_gles2_release`).
//!
//! Журнал провайдера: сколько раз спросили идентичность, сколько раз выдали дескриптор (каждый
//! — это dup fd) и какие кадры вернул ему рендер.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "тестовый стенд: громкие падения"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceIdentityLookup,
    PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
    PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
    FrameResourceDescriptor, FrameResourceHandle, FrameResourceIdentity, HostPlanarFrameDescriptor,
    HostPlaneDescriptor, HostPlaneRole, VideoDecoderDiagnosticEvent, VideoDecoderThreadHandle,
};

use super::nv12_buffers::TestNv12;

/// Что провайдер отвечает на запрос идентичности.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IdentityReply {
    Ready,
    /// Провайдер идентичность не отдаёт (как провайдер без поддержки).
    Unsupported,
    Busy,
    Missing,
    Fatal,
}

/// Что провайдер отвечает на запрос дескриптора.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DescriptorReply {
    DmaBuf,
    /// Дескриптор памяти процесса там, где ждали DMA-BUF.
    HostPlanar,
    Busy,
    Missing,
    Fatal,
}

/// Общее состояние провайдера: ответы, счётчики, журнал возвращённых кадров.
pub struct ProviderState {
    nv12: TestNv12,
    resource_id: u64,
    identity_reply: Mutex<IdentityReply>,
    descriptor_reply: Mutex<DescriptorReply>,
    identity_lookups: AtomicUsize,
    descriptor_lookups: AtomicUsize,
    released: Mutex<Vec<u64>>,
    released_changed: Condvar,
}

impl ProviderState {
    pub fn new(nv12: TestNv12, resource_id: u64) -> Arc<Self> {
        Arc::new(Self {
            nv12,
            resource_id,
            identity_reply: Mutex::new(IdentityReply::Ready),
            descriptor_reply: Mutex::new(DescriptorReply::DmaBuf),
            identity_lookups: AtomicUsize::new(0),
            descriptor_lookups: AtomicUsize::new(0),
            released: Mutex::new(Vec::new()),
            released_changed: Condvar::new(),
        })
    }

    pub fn reply_to_identity(&self, reply: IdentityReply) {
        *self.identity_reply.lock().unwrap() = reply;
    }

    pub fn reply_to_descriptor(&self, reply: DescriptorReply) {
        *self.descriptor_reply.lock().unwrap() = reply;
    }

    pub fn identity_lookups(&self) -> usize {
        self.identity_lookups.load(Ordering::SeqCst)
    }

    /// Сколько дескрипторов (dup fd) выдано.
    pub fn descriptor_lookups(&self) -> usize {
        self.descriptor_lookups.load(Ordering::SeqCst)
    }

    /// Кадры, которые рендер вернул провайдеру, в порядке возврата.
    pub fn released_handles(&self) -> Vec<u64> {
        self.released.lock().unwrap().clone()
    }

    /// Ждёт, пока вернётся не меньше `count` кадров.
    pub fn wait_for_release_count(&self, count: usize, timeout: Duration) -> bool {
        let released = self.released.lock().unwrap();
        let (released, _) = self
            .released_changed
            .wait_timeout_while(released, timeout, |released| released.len() < count)
            .unwrap();
        released.len() >= count
    }
}

/// Провайдер над общим состоянием.
pub struct ScriptedProvider(pub Arc<ProviderState>);

impl PresentFrameResourceProvider for ScriptedProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn try_resource_identity_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceIdentityLookup {
        let state = &self.0;
        state.identity_lookups.fetch_add(1, Ordering::SeqCst);
        let resource_pool_lock_wait = Duration::ZERO;
        match *state.identity_reply.lock().unwrap() {
            IdentityReply::Ready => PresentFrameResourceIdentityLookup::Ready {
                identity: FrameResourceIdentity::DmaBuf(Arc::new(
                    state.nv12.identity(state.resource_id),
                )),
                resource_pool_lock_wait,
            },
            IdentityReply::Unsupported => PresentFrameResourceIdentityLookup::Unsupported,
            IdentityReply::Busy => PresentFrameResourceIdentityLookup::Busy {
                resource_pool_lock_wait,
            },
            IdentityReply::Missing => PresentFrameResourceIdentityLookup::Missing {
                resource_pool_lock_wait,
            },
            IdentityReply::Fatal => PresentFrameResourceIdentityLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let state = &self.0;
        state.descriptor_lookups.fetch_add(1, Ordering::SeqCst);
        let resource_pool_lock_wait = Duration::ZERO;
        match *state.descriptor_reply.lock().unwrap() {
            DescriptorReply::DmaBuf => PresentFrameResourceDescriptorLookup::Ready {
                descriptor: FrameResourceDescriptor::DmaBuf(
                    state.nv12.descriptor(state.resource_id),
                ),
                resource_pool_lock_wait,
            },
            DescriptorReply::HostPlanar => PresentFrameResourceDescriptorLookup::Ready {
                descriptor: FrameResourceDescriptor::HostPlanar(host_planar_descriptor()),
                resource_pool_lock_wait,
            },
            DescriptorReply::Busy => PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait,
            },
            DescriptorReply::Missing => PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait,
            },
            DescriptorReply::Fatal => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.0.released.lock().unwrap().push(handle.0);
        self.0.released_changed.notify_all();
    }
}

/// Минимальный дескриптор программного кадра 2×2 (Y, U, V по одному байту на плоскость).
fn host_planar_descriptor() -> HostPlanarFrameDescriptor {
    let plane = |role, offset| HostPlaneDescriptor {
        role,
        offset,
        stride: 2,
        visible_width: 2,
        visible_height: 2,
        bytes_per_sample: 1,
    };
    HostPlanarFrameDescriptor::from_owned_buffer(
        vec![0; 12],
        vec![
            plane(HostPlaneRole::Luma, 0),
            plane(HostPlaneRole::ChromaU, 4),
            plane(HostPlaneRole::ChromaV, 8),
        ],
    )
}

/// Декодер-пустышка: нужен только как носитель провайдера под обёрткой освобождения.
pub struct ProviderOnlyDecoder {
    pub provider: PresentFrameResourceProviderHandle,
}

impl VideoDecoderThreadHandle for ProviderOnlyDecoder {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "provider-only-test-decoder"
    }

    fn send_packet(&self, _packet: DecodePacket) -> Result<(), DecodeSendError> {
        Err(DecodeSendError::Fatal(DecodeThreadError::new(
            "test decoder takes no packets",
        )))
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.provider.release_frame(handle);
    }

    fn try_recv_frame(&self) -> Option<DecodedFrame> {
        None
    }

    fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn resource_provider(&self) -> Self::ResourceProvider {
        self.provider.clone()
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        None
    }

    fn packet_queue_depth(&self) -> usize {
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        0
    }
}
