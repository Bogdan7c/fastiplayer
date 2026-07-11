mod descriptor;
mod dma_buf_validation;
mod host_planar_validation;

use anyhow::{Context, bail, ensure};
use video_frame_contract::{HardwareFrameHandle, VideoFrameContract, VideoFrameTransferPath};

pub use descriptor::{
    DmaBufDescriptorRejection, DmaBufFrameDescriptor, DmaBufFrameExportLayout,
    DmaBufLayerDescriptor, DmaBufObjectDescriptor, DmaBufObjectIdentity, FrameResourceDescriptor,
    FrameResourceHandle, HostPlanarFrameDescriptor, HostPlanarFrameOwner, HostPlanarOwnedBuffer,
    HostPlanarVisiblePlaneBlock, HostPlaneDescriptor, HostPlaneRole,
};
pub use dma_buf_validation::validate_dma_buf_descriptor_import_topology;

use dma_buf_validation::validate_dma_buf_descriptor_against_contract;
use host_planar_validation::validate_host_planar_descriptor_against_contract;

pub fn validate_resource_descriptor_against_contract(
    contract: VideoFrameContract,
    coded_width: u32,
    coded_height: u32,
    descriptor: &FrameResourceDescriptor,
) -> anyhow::Result<()> {
    contract
        .validate()
        .with_context(|| format!("invalid video frame contract: {contract}"))?;
    ensure!(
        coded_width > 0 && coded_height > 0,
        "resource validation requires positive coded size, got {coded_width}x{coded_height}"
    );

    match (contract.transfer_path, descriptor) {
        (
            VideoFrameTransferPath::HardwareZeroCopy {
                handle:
                    HardwareFrameHandle::DmaBuf {
                        image_layout: expected_layout,
                    },
            },
            FrameResourceDescriptor::DmaBuf(dma_buf_descriptor),
        ) => validate_dma_buf_descriptor_against_contract(
            expected_layout,
            coded_width,
            coded_height,
            dma_buf_descriptor,
        ),
        (
            VideoFrameTransferPath::HardwareZeroCopy { .. },
            FrameResourceDescriptor::HostPlanar(_),
        ) => bail!("hardware zero-copy contract requires DMA-BUF descriptor, got host-planar"),
        (VideoFrameTransferPath::SoftwareHostUpload, FrameResourceDescriptor::HostPlanar(host)) => {
            validate_host_planar_descriptor_against_contract(
                contract.pixel_layout,
                coded_width,
                coded_height,
                host,
            )
        }
        (VideoFrameTransferPath::SoftwareHostUpload, FrameResourceDescriptor::DmaBuf(_)) => {
            bail!("software host-upload contract requires host-planar descriptor, got DMA-BUF")
        }
    }
}

#[cfg(test)]
mod tests;
