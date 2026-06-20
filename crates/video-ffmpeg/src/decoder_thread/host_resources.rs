use std::collections::HashMap;
use std::sync::TryLockError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderLookup,
};
use video_core::{
    FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor, HostPlanarFrameOwner,
    HostPlaneDescriptor, HostPlaneRole, HostUploadResourceSnapshot,
    validate_resource_descriptor_against_contract,
};
use video_frame_contract::{
    FrameBitDepth, FrameChromaSubsampling, VideoFramePixelLayout, VideoFrameTransferPath,
};

use crate::ffi::error::FfmpegError;
use crate::ffi::frame::OwnedAvFrame;
use crate::ffi::pixel_format::SoftwarePixelFormat;

use super::FfmpegDecoderThreadError;

/// Shared provider over AVFrame-backed host-planar resources.
#[derive(Debug, Clone)]
pub(super) struct FfmpegHostResourceProvider {
    /// Shared inner state is cloned into both decoder handle and worker thread.
    inner: Arc<FfmpegHostResourceProviderInner>,
}

/// Mutable resource table плюс counters, hidden behind the provider boundary.
#[derive(Debug)]
struct FfmpegHostResourceProviderInner {
    /// Provider-owned resources that stay alive until renderer calls release.
    table: Mutex<FfmpegHostResourceTable>,

    /// Coalescing wake-up signalled whenever a pool slot is released, so the
    /// decoder worker can resume reception promptly without polling.
    release_notify: Sender<()>,

    /// Upper bound for simultaneously retained host frames.
    upload_slots_capacity: usize,

    /// Cumulative failures while creating/publishing host-upload resources.
    upload_failures: AtomicU64,
}

/// Resource table keyed by neutral opaque frame handles.
#[derive(Debug)]
struct FfmpegHostResourceTable {
    /// Next never-reused handle value for this provider lifetime.
    next_handle: u64,

    /// Active resources still owned by the provider.
    entries: HashMap<FrameResourceHandle, FfmpegHostResourceEntry>,
}

/// One provider-owned resource entry.
#[derive(Debug)]
struct FfmpegHostResourceEntry {
    /// Generation is diagnostic ownership context; release remains handle-based.
    _generation: u64,

    /// Neutral descriptor whose owner holds the refcounted AVFrame alive.
    descriptor: FrameResourceDescriptor,
}

/// Result of moving one received AVFrame into the provider table.
#[derive(Debug)]
pub(super) struct FfmpegHostResourcePublication {
    /// Opaque handle stored in `DecodedFrame`.
    pub(super) handle: FrameResourceHandle,

    /// Actual coded width read from the received AVFrame.
    pub(super) width: u32,

    /// Actual coded height read from the received AVFrame.
    pub(super) height: u32,
}

impl FfmpegHostResourceProvider {
    /// Создаёт provider с bounded числом host-upload slots.
    pub(super) fn new(upload_slots_capacity: usize, release_notify: Sender<()>) -> Self {
        let upload_slots_capacity = upload_slots_capacity.max(1);

        Self {
            inner: Arc::new(FfmpegHostResourceProviderInner {
                table: Mutex::new(FfmpegHostResourceTable {
                    next_handle: 1,
                    entries: HashMap::new(),
                }),
                release_notify,
                upload_slots_capacity,
                upload_failures: AtomicU64::new(0),
            }),
        }
    }

    /// Возвращает число свободных host-upload slots прямо сейчас.
    ///
    /// Это hard-предел одновременно живущих decoded frames: каждый занятый slot
    /// держит refcounted AVFrame до `release_frame`. Decode loop использует это
    /// как receive budget, чтобы никогда не публиковать больше кадров, чем
    /// помещается в пул, и не упираться в fatal table-full.
    pub(super) fn free_slots(&self) -> usize {
        let in_flight = self
            .inner
            .table
            .lock()
            .map(|table| table.entries.len())
            .unwrap_or(self.inner.upload_slots_capacity);
        self.inner.upload_slots_capacity.saturating_sub(in_flight)
    }

    /// Converts a refcounted AVFrame into a provider-owned host-planar resource.
    pub(super) fn insert_frame(
        &self,
        generation: u64,
        frame: OwnedAvFrame,
        expected_contract: video_frame_contract::VideoFrameContract,
    ) -> Result<FfmpegHostResourcePublication, FfmpegDecoderThreadError> {
        let publication = self.insert_frame_inner(generation, frame, expected_contract);

        if publication.is_err() {
            self.record_upload_failure();
        }

        publication
    }

    /// Internal implementation split out so failure accounting stays in one place.
    fn insert_frame_inner(
        &self,
        generation: u64,
        frame: OwnedAvFrame,
        expected_contract: video_frame_contract::VideoFrameContract,
    ) -> Result<FfmpegHostResourcePublication, FfmpegDecoderThreadError> {
        let (descriptor, width, height) = avframe_host_planar_descriptor(frame, expected_contract)?;

        validate_resource_descriptor_against_contract(
            expected_contract,
            width,
            height,
            &descriptor,
        )
        .map_err(|error| {
            invalid_avframe_resource(
                "AVFrame HostPlanar descriptor validation",
                error.to_string(),
            )
        })?;

        let mut table = self.inner.table.lock().map_err(|_| {
            invalid_avframe_resource(
                "AVFrame HostPlanar resource table lock",
                "resource table mutex is poisoned".to_owned(),
            )
        })?;

        if table.entries.len() >= self.inner.upload_slots_capacity {
            return Err(FfmpegDecoderThreadError::ProtocolViolation {
                reason: format!(
                    "FFmpeg host-upload resource table is full: {}/{} slots are occupied",
                    table.entries.len(),
                    self.inner.upload_slots_capacity
                ),
            });
        }

        let handle = table.allocate_handle()?;
        table.entries.insert(
            handle,
            FfmpegHostResourceEntry {
                _generation: generation,
                descriptor,
            },
        );

        Ok(FfmpegHostResourcePublication {
            handle,
            width,
            height,
        })
    }

    /// Snapshot used by the neutral software host-upload backpressure boundary.
    pub(super) fn snapshot(&self, host_frames_ready: usize) -> HostUploadResourceSnapshot {
        let host_frames_in_flight = self
            .inner
            .table
            .lock()
            .map(|table| table.entries.len())
            .unwrap_or(self.inner.upload_slots_capacity);
        let upload_slots_free = self
            .inner
            .upload_slots_capacity
            .saturating_sub(host_frames_in_flight);

        HostUploadResourceSnapshot {
            host_frames_ready,
            host_frames_in_flight,
            upload_slots_capacity: self.inner.upload_slots_capacity,
            upload_slots_free,
            upload_failures: self.inner.upload_failures.load(Ordering::Relaxed),
        }
    }

    /// Counts resource creation/publish failures without changing error semantics.
    pub(super) fn record_upload_failure(&self) {
        self.inner.upload_failures.fetch_add(1, Ordering::Relaxed);
    }
}

impl FfmpegHostResourceTable {
    /// Allocates the next opaque handle without reusing released values.
    fn allocate_handle(&mut self) -> Result<FrameResourceHandle, FfmpegDecoderThreadError> {
        let handle = FrameResourceHandle(self.next_handle);
        self.next_handle = self.next_handle.checked_add(1).ok_or_else(|| {
            FfmpegDecoderThreadError::ProtocolViolation {
                reason: "FFmpeg host-upload resource handle counter overflowed".to_owned(),
            }
        })?;
        Ok(handle)
    }
}

impl PresentFrameResourceProvider for FfmpegHostResourceProvider {
    fn resource_lookup(&self, handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        let lock_start = Instant::now();
        match self.inner.table.lock() {
            Ok(table) => resource_lookup_from_table(&table, handle, lock_start.elapsed()),
            Err(_) => PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
        }
    }

    fn try_resource_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        let lock_start = Instant::now();
        match self.inner.table.try_lock() {
            Ok(table) => resource_lookup_from_table(&table, handle, lock_start.elapsed()),
            Err(TryLockError::WouldBlock) => PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
            Err(TryLockError::Poisoned(_)) => PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
        }
    }

    fn resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let lock_start = Instant::now();
        match self.inner.table.lock() {
            Ok(table) => descriptor_lookup_from_table(&table, handle, lock_start.elapsed()),
            Err(_) => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let lock_start = Instant::now();
        match self.inner.table.try_lock() {
            Ok(table) => descriptor_lookup_from_table(&table, handle, lock_start.elapsed()),
            Err(TryLockError::WouldBlock) => PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
            Err(TryLockError::Poisoned(_)) => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
        }
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        let removed = self
            .inner
            .table
            .lock()
            .map(|mut table| table.entries.remove(&handle).is_some())
            .unwrap_or(false);
        if removed {
            // Coalescing wake-up: ok to drop when one is already pending.
            let _ = self.inner.release_notify.try_send(());
        }
    }
}

fn resource_lookup_from_table(
    table: &FfmpegHostResourceTable,
    handle: FrameResourceHandle,
    resource_pool_lock_wait: Duration,
) -> PresentFrameResourceProviderLookup {
    if table.entries.contains_key(&handle) {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait,
        }
    } else {
        PresentFrameResourceProviderLookup::Missing {
            resource_pool_lock_wait,
        }
    }
}

fn descriptor_lookup_from_table(
    table: &FfmpegHostResourceTable,
    handle: FrameResourceHandle,
    resource_pool_lock_wait: Duration,
) -> PresentFrameResourceDescriptorLookup {
    let Some(entry) = table.entries.get(&handle) else {
        return PresentFrameResourceDescriptorLookup::Missing {
            resource_pool_lock_wait,
        };
    };

    match entry.descriptor.try_clone_for_lookup() {
        Ok(descriptor) => PresentFrameResourceDescriptorLookup::Ready {
            descriptor,
            resource_pool_lock_wait,
        },
        Err(_) => PresentFrameResourceDescriptorLookup::Fatal {
            resource_pool_lock_wait,
        },
    }
}

/// HostPlanar owner that keeps a refcounted AVFrame alive behind video-core API.
#[derive(Debug)]
struct AvFrameHostPlanarOwner {
    /// Separate `av_frame_ref` owned by the resource table/descriptor clones.
    frame: OwnedAvFrame,
}

// SAFETY: after publication this owner never mutates the wrapped AVFrame. Its
// safe API exposes only immutable row slices, and Drop only releases FFmpeg's
// refcounted buffers when the last descriptor clone disappears.
unsafe impl Sync for AvFrameHostPlanarOwner {}

impl HostPlanarFrameOwner for AvFrameHostPlanarOwner {
    fn visible_row_bytes(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        if plane.offset != 0 {
            return Err(anyhow::anyhow!(
                "AVFrame-backed host-planar {:?} plane uses non-zero owner offset {}",
                plane.role,
                plane.offset
            ));
        }

        let row_index = usize::try_from(row_index).map_err(|_| {
            anyhow::anyhow!(
                "AVFrame-backed host-planar {:?} row index {} does not fit usize",
                plane.role,
                row_index
            )
        })?;

        self.frame
            .plane_row_data(plane_index, row_index, visible_row_bytes)
            .map_err(|error| anyhow::anyhow!(error))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "AVFrame-backed host-planar {:?} plane {} has null data pointer",
                    plane.role,
                    plane_index
                )
            })
    }

    fn visible_plane_block(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        block_len: usize,
    ) -> anyhow::Result<&[u8]> {
        if plane.offset != 0 {
            return Err(anyhow::anyhow!(
                "AVFrame-backed host-planar {:?} plane uses non-zero owner offset {}",
                plane.role,
                plane.offset
            ));
        }

        // block_len посчитан descriptor-ом из plane.stride; убеждаемся, что это
        // тот же linesize, что у AVFrame, иначе срез мог бы выйти за plane buffer.
        let line_size = self.frame.linesize(plane_index).ok_or_else(|| {
            anyhow::anyhow!(
                "AVFrame-backed host-planar {:?} plane {} has no linesize",
                plane.role,
                plane_index
            )
        })?;
        let line_size = usize::try_from(line_size).map_err(|_| {
            anyhow::anyhow!(
                "AVFrame-backed host-planar {:?} plane {} has negative linesize {}",
                plane.role,
                plane_index,
                line_size
            )
        })?;
        if line_size != plane.stride {
            return Err(anyhow::anyhow!(
                "AVFrame-backed host-planar {:?} plane {} linesize {} does not match descriptor stride {}",
                plane.role,
                plane_index,
                line_size,
                plane.stride
            ));
        }

        self.frame
            .plane_block_data(plane_index, block_len)
            .map_err(|error| anyhow::anyhow!(error))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "AVFrame-backed host-planar {:?} plane {} has null data pointer",
                    plane.role,
                    plane_index
                )
            })
    }
}

fn avframe_host_planar_descriptor(
    frame: OwnedAvFrame,
    expected_contract: video_frame_contract::VideoFrameContract,
) -> Result<(FrameResourceDescriptor, u32, u32), FfmpegDecoderThreadError> {
    if expected_contract.transfer_path != VideoFrameTransferPath::SoftwareHostUpload {
        return Err(invalid_avframe_resource(
            "AVFrame HostPlanar descriptor",
            format!(
                "expected SoftwareHostUpload contract, got {}",
                expected_contract.transfer_path
            ),
        ));
    }

    let frame_format = frame.software_format().ok_or_else(|| {
        invalid_avframe_resource(
            "AVFrame pixel format",
            format!(
                "AVFrame format code {} is not a supported v1 software planar YUV format",
                frame.raw_format_code()
            ),
        )
    })?;
    ensure_frame_format_matches_contract(frame_format, expected_contract.pixel_layout)?;

    let width = positive_avframe_dimension("width", frame.width())?;
    let height = positive_avframe_dimension("height", frame.height())?;
    let planes =
        avframe_host_plane_descriptors(&frame, expected_contract.pixel_layout, width, height)?;
    let owner = Arc::new(AvFrameHostPlanarOwner { frame });
    let descriptor = HostPlanarFrameDescriptor::new(owner, planes);

    Ok((
        FrameResourceDescriptor::HostPlanar(descriptor),
        width,
        height,
    ))
}

fn ensure_frame_format_matches_contract(
    frame_format: SoftwarePixelFormat,
    expected_layout: VideoFramePixelLayout,
) -> Result<(), FfmpegDecoderThreadError> {
    let actual_layout = frame_format.frame_pixel_layout();
    if actual_layout != expected_layout {
        return Err(invalid_avframe_resource(
            "AVFrame pixel format",
            format!(
                "decoded AVFrame layout {} does not match selected contract {}",
                actual_layout, expected_layout
            ),
        ));
    }

    Ok(())
}

fn positive_avframe_dimension(
    name: &'static str,
    value: i32,
) -> Result<u32, FfmpegDecoderThreadError> {
    u32::try_from(value)
        .ok()
        .filter(|dimension| *dimension > 0)
        .ok_or_else(|| {
            invalid_avframe_resource(
                "AVFrame dimensions",
                format!("AVFrame {name} must be positive, got {value}"),
            )
        })
}

fn avframe_host_plane_descriptors(
    frame: &OwnedAvFrame,
    pixel_layout: VideoFramePixelLayout,
    width: u32,
    height: u32,
) -> Result<Vec<HostPlaneDescriptor>, FfmpegDecoderThreadError> {
    let bytes_per_sample = host_planar_bytes_per_sample(pixel_layout)?;
    let (chroma_width, chroma_height) = host_planar_chroma_size(pixel_layout, width, height)?;
    let plane_specs = [
        (HostPlaneRole::Luma, width, height),
        (HostPlaneRole::ChromaU, chroma_width, chroma_height),
        (HostPlaneRole::ChromaV, chroma_width, chroma_height),
    ];

    plane_specs
        .into_iter()
        .enumerate()
        .map(|(plane_index, (role, visible_width, visible_height))| {
            let stride = positive_avframe_linesize(frame, plane_index, role)?;

            Ok(HostPlaneDescriptor {
                role,
                offset: 0,
                stride,
                visible_width,
                visible_height,
                bytes_per_sample,
            })
        })
        .collect()
}

fn positive_avframe_linesize(
    frame: &OwnedAvFrame,
    plane_index: usize,
    role: HostPlaneRole,
) -> Result<usize, FfmpegDecoderThreadError> {
    let line_size = frame.linesize(plane_index).ok_or_else(|| {
        invalid_avframe_resource(
            "AVFrame linesize",
            format!(
                "AVFrame {:?} plane index {} is outside linesize table",
                role, plane_index
            ),
        )
    })?;

    usize::try_from(line_size)
        .ok()
        .filter(|line_size| *line_size > 0)
        .ok_or_else(|| {
            invalid_avframe_resource(
                "AVFrame linesize",
                format!(
                    "AVFrame {:?} plane {} has unsupported non-positive linesize {}",
                    role, plane_index, line_size
                ),
            )
        })
}

fn host_planar_bytes_per_sample(
    pixel_layout: VideoFramePixelLayout,
) -> Result<usize, FfmpegDecoderThreadError> {
    match pixel_layout.bit_depth() {
        Some(FrameBitDepth::Eight) => Ok(1),
        Some(FrameBitDepth::Ten | FrameBitDepth::Twelve) => Ok(2),
        None => Err(invalid_avframe_resource(
            "AVFrame HostPlanar layout",
            format!("{pixel_layout} is not a host-planar YUV layout"),
        )),
    }
}

fn host_planar_chroma_size(
    pixel_layout: VideoFramePixelLayout,
    width: u32,
    height: u32,
) -> Result<(u32, u32), FfmpegDecoderThreadError> {
    match pixel_layout.chroma() {
        Some(FrameChromaSubsampling::Yuv420) => {
            Ok((half_rounded_up(width), half_rounded_up(height)))
        }
        Some(FrameChromaSubsampling::Yuv422) => Ok((half_rounded_up(width), height)),
        Some(FrameChromaSubsampling::Yuv444) => Ok((width, height)),
        None => Err(invalid_avframe_resource(
            "AVFrame HostPlanar layout",
            format!("{pixel_layout} is not a planar YUV layout"),
        )),
    }
}

const fn half_rounded_up(value: u32) -> u32 {
    (value / 2) + (value % 2)
}

pub(super) fn invalid_avframe_resource(
    operation: &'static str,
    details: String,
) -> FfmpegDecoderThreadError {
    FfmpegDecoderThreadError::Ffi(FfmpegError::InvalidInput { operation, details })
}
