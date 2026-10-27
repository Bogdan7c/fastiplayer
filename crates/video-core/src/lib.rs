pub mod decoder;
pub mod decoder_thread;
pub mod diagnostics;
pub mod resource;
pub mod sync;

pub use decoder::{DecodedFrame, DecodedPixelFormat, FrameMemoryPath, VideoDecoder};
pub use decoder_thread::{
    DecodeBackpressureReason, DecodePacket, DecodeSendError, DecodeThreadError,
    DecoderResourceSnapshot, HostUploadBackpressureReason, HostUploadResourceSnapshot,
    HostUploadResourceSnapshotStatus, SoftwareDecodeThreadBudget, VideoDecoderActivityEpoch,
    VideoDecoderActivityNotifier, VideoDecoderActivitySnapshot, VideoDecoderActivitySubscription,
    VideoDecoderActivityUnavailableReason, VideoDecoderActivityWaitOutcome,
    VideoDecoderControlBackpressureReason, VideoDecoderControlChannelPressureSnapshot,
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
    VideoDecoderThreadConfig, VideoDecoderThreadHandle, VideoPrerollOutputFloor,
    VideoPrerollOutputFloorClear, VideoPrerollOutputFloorResult, VideoStreamConfigRejection,
    VideoStreamConfigResult, VideoStreamDecodeConfig, VideoStreamPacketization,
};
pub use diagnostics::{
    VideoDecoderDiagnosticEvent, VideoDecoderDropReason, VideoFrameDiagnostics,
    VideoFramePublishPressureDiagnostics, VideoFrameTimingDiagnostics,
    VideoResourcePoolDiagnostics,
};
#[cfg(target_os = "linux")]
pub use resource::SealedMemfdRegion;
pub use resource::{
    DmaBufDescriptorRejection, DmaBufFrameDescriptor, DmaBufFrameExportLayout,
    DmaBufFrameFormatSignature, DmaBufFrameIdentity, DmaBufLayerDescriptor, DmaBufObjectDescriptor,
    DmaBufObjectIdentity, DmaBufObjectIdentityDescriptor, FrameResourceDescriptor,
    FrameResourceHandle, FrameResourceIdentity, HOST_IMPORTABLE_BLOCK_ALIGNMENT,
    HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT, HostFrameMemoryKind, HostImportableMemoryBlock,
    HostMemoryBlockId, HostMemoryRegion, HostPlanarFrameDescriptor, HostPlanarFrameOwner,
    HostPlanarImportableMemory, HostPlanarOwnedBuffer, HostPlanarVisiblePlaneBlock,
    HostPlaneDescriptor, HostPlaneRole, validate_dma_buf_descriptor_against_frame_contract,
    validate_dma_buf_descriptor_import_topology, validate_dma_buf_identity_against_frame_contract,
    validate_resource_descriptor_against_contract,
};
pub use sync::{AvSync, FrameAction};
