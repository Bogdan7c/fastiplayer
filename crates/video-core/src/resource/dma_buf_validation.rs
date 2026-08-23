use anyhow::Context;
use video_frame_contract::{
    DmaBufImageLayout, HardwareFrameHandle, VideoFrameContract, VideoFrameTransferPath,
};

use super::descriptor::{
    DmaBufDescriptorRejection, DmaBufFrameDescriptor, DmaBufFrameExportLayout,
};
/// Проверяет object/layer topology до late renderer import-а.
///
/// `SeparateLayers` намеренно допускает разные objects у разных layers: renderer
/// импортирует такие luma/chroma layers независимо. Ограничение относится только к
/// composed multi-planar image, planes которого требуют нескольких memory binds.
pub fn validate_dma_buf_descriptor_import_topology(
    descriptor: &DmaBufFrameDescriptor,
) -> Result<(), DmaBufDescriptorRejection> {
    if descriptor.objects.is_empty() {
        return Err(DmaBufDescriptorRejection::AbsentObjects);
    }
    if descriptor.layers.is_empty() {
        return Err(DmaBufDescriptorRejection::AbsentLayers);
    }

    let mut composed_first_object_index = None;
    for (layer_index, layer) in descriptor.layers.iter().enumerate() {
        let plane_count = usize::try_from(layer.num_planes).unwrap_or(usize::MAX);
        if !(1..=layer.object_index.len()).contains(&plane_count) {
            return Err(DmaBufDescriptorRejection::InvalidPlaneCount {
                layer_index,
                plane_count: layer.num_planes,
            });
        }

        for (plane_index, raw_object_index) in layer.object_index[..plane_count]
            .iter()
            .copied()
            .enumerate()
        {
            let object_index = usize::from(raw_object_index);
            if object_index >= descriptor.objects.len() {
                return Err(DmaBufDescriptorRejection::ObjectIndexOutOfBounds {
                    layer_index,
                    plane_index,
                    object_index,
                    object_count: descriptor.objects.len(),
                });
            }

            if descriptor.export_layout == DmaBufFrameExportLayout::ComposedLayers {
                match composed_first_object_index {
                    Some(first_object_index) if first_object_index != object_index => {
                        return Err(DmaBufDescriptorRejection::UnsupportedComposedMultiObject {
                            first_object_index,
                            conflicting_object_index: object_index,
                        });
                    }
                    Some(_) => {}
                    None => composed_first_object_index = Some(object_index),
                }
            }
        }
    }

    Ok(())
}

/// Проверяет DMA-BUF descriptor против полного decoded frame contract-а до unsafe import-а.
///
/// Boundary намеренно renderer-neutral: provider descriptor сверяется с обещанными
/// transfer path, image layout и coded dimensions без знания Vulkan/WGPU реализации.
pub fn validate_dma_buf_descriptor_against_frame_contract(
    contract: VideoFrameContract,
    coded_width: u32,
    coded_height: u32,
    descriptor: &DmaBufFrameDescriptor,
) -> Result<(), DmaBufDescriptorRejection> {
    if let Err(error) = contract.validate() {
        return Err(DmaBufDescriptorRejection::InvalidFrameContract {
            reason: error.to_string(),
        });
    }
    if coded_width == 0 || coded_height == 0 {
        return Err(DmaBufDescriptorRejection::InvalidCodedSize {
            coded_width,
            coded_height,
        });
    }

    let expected_layout = match contract.transfer_path {
        VideoFrameTransferPath::HardwareZeroCopy {
            handle: HardwareFrameHandle::DmaBuf { image_layout },
        } => image_layout,
        VideoFrameTransferPath::SoftwareHostUpload => {
            return Err(DmaBufDescriptorRejection::FrameContractRequiresHostUpload);
        }
    };

    validate_dma_buf_descriptor_against_expected_contract(
        expected_layout,
        coded_width,
        coded_height,
        descriptor,
    )
}

pub(super) fn validate_dma_buf_descriptor_against_contract(
    expected_layout: DmaBufImageLayout,
    coded_width: u32,
    coded_height: u32,
    descriptor: &DmaBufFrameDescriptor,
) -> anyhow::Result<()> {
    match validate_dma_buf_descriptor_against_expected_contract(
        expected_layout,
        coded_width,
        coded_height,
        descriptor,
    ) {
        Ok(()) => Ok(()),
        Err(rejection) if rejection_is_import_topology_error(&rejection) => {
            Err(rejection).context("DMA-BUF descriptor import topology is unsupported")
        }
        Err(rejection) => Err(rejection.into()),
    }
}

/// Общая typed проверка topology, export layout и coded dimensions.
fn validate_dma_buf_descriptor_against_expected_contract(
    expected_layout: DmaBufImageLayout,
    coded_width: u32,
    coded_height: u32,
    descriptor: &DmaBufFrameDescriptor,
) -> Result<(), DmaBufDescriptorRejection> {
    validate_dma_buf_descriptor_import_topology(descriptor)?;
    let actual_layout = dma_buf_export_layout_to_image_layout(descriptor.export_layout);
    if actual_layout != expected_layout {
        return Err(DmaBufDescriptorRejection::ImageLayoutMismatch {
            expected: expected_layout,
            actual: actual_layout,
        });
    }
    if descriptor.width != coded_width || descriptor.height != coded_height {
        return Err(DmaBufDescriptorRejection::CodedSizeMismatch {
            expected_width: coded_width,
            expected_height: coded_height,
            actual_width: descriptor.width,
            actual_height: descriptor.height,
        });
    }
    Ok(())
}

/// Отличает прежние topology errors, для которых публичный anyhow boundary сохраняет context.
fn rejection_is_import_topology_error(rejection: &DmaBufDescriptorRejection) -> bool {
    matches!(
        rejection,
        DmaBufDescriptorRejection::AbsentObjects
            | DmaBufDescriptorRejection::AbsentLayers
            | DmaBufDescriptorRejection::InvalidPlaneCount { .. }
            | DmaBufDescriptorRejection::ObjectIndexOutOfBounds { .. }
            | DmaBufDescriptorRejection::UnsupportedComposedMultiObject { .. }
    )
}

const fn dma_buf_export_layout_to_image_layout(
    export_layout: DmaBufFrameExportLayout,
) -> DmaBufImageLayout {
    match export_layout {
        DmaBufFrameExportLayout::ComposedLayers => DmaBufImageLayout::ComposedLayers,
        DmaBufFrameExportLayout::SeparateLayers => DmaBufImageLayout::SeparateLayers,
    }
}
