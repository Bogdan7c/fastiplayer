//! Стенд: кадры, дескрипторы и подставной провайдер ресурсов декодера.

use std::fs::File;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use gles2_egui::DrmFourcc;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceIdentityLookup,
    PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
    PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodedFrame, DmaBufFrameDescriptor, DmaBufFrameExportLayout, DmaBufLayerDescriptor,
    DmaBufObjectDescriptor, DmaBufObjectIdentity, FrameResourceDescriptor, FrameResourceHandle,
    FrameResourceIdentity, VideoFrameDiagnostics,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use crate::dma_buf_frame::passport::{FOURCC_NV12, FOURCC_P010};

pub(super) const WIDTH: u32 = 32;
pub(super) const HEIGHT: u32 = 16;
pub(super) const PITCH: u32 = 256;
/// Модификатор тестовых буферов (не `LINEAR`, чтобы он не терялся незаметно).
pub(super) const MODIFIER: u64 = 0x0100_0000_0000_0001;

/// Новый fd на `/dev/null`: настоящий дескриптор, который закрывается при drop.
fn some_fd() -> OwnedFd {
    OwnedFd::from(File::open("/dev/null").expect("/dev/null opens"))
}

pub(super) fn object(inode: u64, modifier: u64) -> DmaBufObjectDescriptor {
    DmaBufObjectDescriptor {
        fd: some_fd(),
        size: PITCH * HEIGHT * 2,
        drm_format_modifier: modifier,
        identity: DmaBufObjectIdentity {
            device: 7,
            inode,
            special_device: 0,
        },
    }
}

/// Composed: один объект, один слой NV12 из двух плоскостей.
pub(super) fn composed_descriptor(resource_id: u64, inode: u64) -> DmaBufFrameDescriptor {
    DmaBufFrameDescriptor {
        resource_id,
        fourcc: FOURCC_NV12,
        export_layout: DmaBufFrameExportLayout::ComposedLayers,
        width: WIDTH,
        height: HEIGHT,
        objects: vec![object(inode, MODIFIER)],
        layers: vec![DmaBufLayerDescriptor {
            drm_format: FOURCC_NV12,
            num_planes: 2,
            object_index: [0, 0, 0, 0],
            offset: [0, PITCH * HEIGHT, 0, 0],
            pitch: [PITCH, PITCH, 0, 0],
        }],
    }
}

/// Separate: два объекта, слои R8 и GR88 по одной плоскости.
pub(super) fn separate_descriptor(resource_id: u64, inode: u64) -> DmaBufFrameDescriptor {
    DmaBufFrameDescriptor {
        resource_id,
        fourcc: FOURCC_NV12,
        export_layout: DmaBufFrameExportLayout::SeparateLayers,
        width: WIDTH,
        height: HEIGHT,
        objects: vec![object(inode, MODIFIER), object(inode + 1, MODIFIER)],
        layers: vec![
            DmaBufLayerDescriptor {
                drm_format: DrmFourcc::R8.0,
                num_planes: 1,
                object_index: [0, 0, 0, 0],
                offset: [64, 0, 0, 0],
                pitch: [PITCH, 0, 0, 0],
            },
            DmaBufLayerDescriptor {
                drm_format: DrmFourcc::GR88.0,
                num_planes: 1,
                object_index: [1, 0, 0, 0],
                offset: [128, 0, 0, 0],
                pitch: [PITCH + 64, 0, 0, 0],
            },
        ],
    }
}

pub(super) fn p010_descriptor(resource_id: u64, inode: u64) -> DmaBufFrameDescriptor {
    DmaBufFrameDescriptor {
        fourcc: FOURCC_P010,
        ..composed_descriptor(resource_id, inode)
    }
}

pub(super) fn contract(layout: DmaBufImageLayout) -> VideoFrameContract {
    VideoFrameContract::dma_buf_nv12(layout)
}

/// Кадр декодера с контрактом `contract` и номером ресурса `handle`.
pub(super) fn frame(contract: VideoFrameContract, handle: u64) -> DecodedFrame {
    DecodedFrame {
        generation: 1,
        pts: Duration::ZERO,
        frame_contract: contract,
        width: WIDTH,
        height: HEIGHT,
        render_width: WIDTH,
        render_height: HEIGHT,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(handle),
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Как отвечает подставной провайдер на lookup.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Reply {
    Ready,
    Busy,
    Missing,
    Fatal,
}

/// Счётчики обращений к подставному провайдеру (читает тест).
#[derive(Default)]
pub(super) struct ProviderCalls {
    pub(super) identity_lookups: AtomicUsize,
    pub(super) descriptor_lookups: AtomicUsize,
}

impl ProviderCalls {
    pub(super) fn identity(&self) -> usize {
        self.identity_lookups.load(Ordering::SeqCst)
    }
    pub(super) fn descriptor(&self) -> usize {
        self.descriptor_lookups.load(Ordering::SeqCst)
    }
}

pub(super) const IDENTITY_LOCK_WAIT: Duration = Duration::from_micros(3);
pub(super) const DESCRIPTOR_LOCK_WAIT: Duration = Duration::from_micros(5);

/// Подставной провайдер: свежий дескриптор на каждый lookup (как dup fd у настоящего).
pub(super) struct FakeProvider {
    pub(super) identity: Option<Reply>,
    pub(super) descriptor: Reply,
    pub(super) make_descriptor: Box<dyn Fn() -> FrameResourceDescriptor + Send + Sync>,
    pub(super) calls: Arc<ProviderCalls>,
}

impl FakeProvider {
    /// Провайдер с идентичностью (`identity: Some`) или без неё (`None` → `Unsupported`).
    pub(super) fn new(
        make_descriptor: impl Fn() -> FrameResourceDescriptor + Send + Sync + 'static,
    ) -> Self {
        Self {
            identity: Some(Reply::Ready),
            descriptor: Reply::Ready,
            make_descriptor: Box::new(make_descriptor),
            calls: Arc::new(ProviderCalls::default()),
        }
    }

    pub(super) fn into_handle(self) -> (PresentFrameResourceProviderHandle, Arc<ProviderCalls>) {
        let calls = Arc::clone(&self.calls);
        (PresentFrameResourceProviderHandle::new(self), calls)
    }
}

impl PresentFrameResourceProvider for FakeProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.calls.descriptor_lookups.fetch_add(1, Ordering::SeqCst);
        let resource_pool_lock_wait = DESCRIPTOR_LOCK_WAIT;
        match self.descriptor {
            Reply::Ready => PresentFrameResourceDescriptorLookup::Ready {
                descriptor: (self.make_descriptor)(),
                resource_pool_lock_wait,
            },
            Reply::Busy => PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait,
            },
            Reply::Missing => PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait,
            },
            Reply::Fatal => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    fn try_resource_identity_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceIdentityLookup {
        self.calls.identity_lookups.fetch_add(1, Ordering::SeqCst);
        let resource_pool_lock_wait = IDENTITY_LOCK_WAIT;
        match self.identity {
            None => PresentFrameResourceIdentityLookup::Unsupported,
            Some(Reply::Ready) => match (self.make_descriptor)() {
                FrameResourceDescriptor::DmaBuf(descriptor) => {
                    PresentFrameResourceIdentityLookup::Ready {
                        identity: FrameResourceIdentity::DmaBuf(Arc::new(descriptor.identity())),
                        resource_pool_lock_wait,
                    }
                }
                FrameResourceDescriptor::HostPlanar(_) => {
                    PresentFrameResourceIdentityLookup::Unsupported
                }
            },
            Some(Reply::Busy) => PresentFrameResourceIdentityLookup::Busy {
                resource_pool_lock_wait,
            },
            Some(Reply::Missing) => PresentFrameResourceIdentityLookup::Missing {
                resource_pool_lock_wait,
            },
            Some(Reply::Fatal) => PresentFrameResourceIdentityLookup::Fatal {
                resource_pool_lock_wait,
            },
        }
    }

    fn release_frame(&self, _handle: FrameResourceHandle) {}
}
