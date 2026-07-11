use anyhow::{Context, ensure};
use video_frame_contract::DmaBufImageLayout;

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
pub(super) fn validate_dma_buf_descriptor_against_contract(
    expected_layout: DmaBufImageLayout,
    coded_width: u32,
    coded_height: u32,
    descriptor: &DmaBufFrameDescriptor,
) -> anyhow::Result<()> {
    validate_dma_buf_descriptor_import_topology(descriptor)
        .context("DMA-BUF descriptor import topology is unsupported")?;
    let actual_layout = dma_buf_export_layout_to_image_layout(descriptor.export_layout);
    ensure!(
        actual_layout == expected_layout,
        "DMA-BUF image layout mismatch: expected {expected_layout}, got {actual_layout}"
    );
    ensure!(
        descriptor.width == coded_width && descriptor.height == coded_height,
        "DMA-BUF coded size mismatch: expected {coded_width}x{coded_height}, got {}x{}",
        descriptor.width,
        descriptor.height
    );
    Ok(())
}

const fn dma_buf_export_layout_to_image_layout(
    export_layout: DmaBufFrameExportLayout,
) -> DmaBufImageLayout {
    match export_layout {
        DmaBufFrameExportLayout::ComposedLayers => DmaBufImageLayout::ComposedLayers,
        DmaBufFrameExportLayout::SeparateLayers => DmaBufImageLayout::SeparateLayers,
    }
}
