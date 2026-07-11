use anyhow::{bail, ensure};
use video_frame_contract::VideoFramePixelLayout;

use super::descriptor::host_plane_visible_row_bytes;
use super::descriptor::{HostPlanarFrameDescriptor, HostPlaneDescriptor, HostPlaneRole};
pub(super) fn validate_host_planar_descriptor_against_contract(
    pixel_layout: VideoFramePixelLayout,
    coded_width: u32,
    coded_height: u32,
    descriptor: &HostPlanarFrameDescriptor,
) -> anyhow::Result<()> {
    let expected_planes = expected_host_planar_planes(pixel_layout, coded_width, coded_height)?;
    ensure!(
        descriptor.planes.len() == expected_planes.len(),
        "host-planar plane count mismatch for {pixel_layout}: expected {}, got {}",
        expected_planes.len(),
        descriptor.planes.len()
    );

    for (plane_index, (plane, expected_plane)) in descriptor
        .planes
        .iter()
        .zip(expected_planes.iter())
        .enumerate()
    {
        validate_host_plane_metadata(pixel_layout, plane, expected_plane)?;
        validate_host_plane_visible_bounds(descriptor, plane_index, plane)?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ExpectedHostPlane {
    role: HostPlaneRole,
    visible_width: u32,
    visible_height: u32,
    bytes_per_sample: usize,
}

#[derive(Debug, Clone, Copy)]
struct ExpectedHostPlanarLayout {
    chroma_width: u32,
    chroma_height: u32,
    bytes_per_sample: usize,
}

fn expected_host_planar_planes(
    pixel_layout: VideoFramePixelLayout,
    coded_width: u32,
    coded_height: u32,
) -> anyhow::Result<Vec<ExpectedHostPlane>> {
    let layout = expected_host_planar_layout(pixel_layout, coded_width, coded_height)?;

    Ok(vec![
        ExpectedHostPlane {
            role: HostPlaneRole::Luma,
            visible_width: coded_width,
            visible_height: coded_height,
            bytes_per_sample: layout.bytes_per_sample,
        },
        ExpectedHostPlane {
            role: HostPlaneRole::ChromaU,
            visible_width: layout.chroma_width,
            visible_height: layout.chroma_height,
            bytes_per_sample: layout.bytes_per_sample,
        },
        ExpectedHostPlane {
            role: HostPlaneRole::ChromaV,
            visible_width: layout.chroma_width,
            visible_height: layout.chroma_height,
            bytes_per_sample: layout.bytes_per_sample,
        },
    ])
}

fn expected_host_planar_layout(
    pixel_layout: VideoFramePixelLayout,
    coded_width: u32,
    coded_height: u32,
) -> anyhow::Result<ExpectedHostPlanarLayout> {
    match pixel_layout {
        VideoFramePixelLayout::Yuv420Planar8 => Ok(ExpectedHostPlanarLayout {
            chroma_width: half_rounded_up(coded_width),
            chroma_height: half_rounded_up(coded_height),
            bytes_per_sample: 1,
        }),
        VideoFramePixelLayout::Yuv420Planar10Le | VideoFramePixelLayout::Yuv420Planar12Le => {
            Ok(ExpectedHostPlanarLayout {
                chroma_width: half_rounded_up(coded_width),
                chroma_height: half_rounded_up(coded_height),
                bytes_per_sample: 2,
            })
        }
        VideoFramePixelLayout::Yuv422Planar8 => Ok(ExpectedHostPlanarLayout {
            chroma_width: half_rounded_up(coded_width),
            chroma_height: coded_height,
            bytes_per_sample: 1,
        }),
        VideoFramePixelLayout::Yuv422Planar10Le | VideoFramePixelLayout::Yuv422Planar12Le => {
            Ok(ExpectedHostPlanarLayout {
                chroma_width: half_rounded_up(coded_width),
                chroma_height: coded_height,
                bytes_per_sample: 2,
            })
        }
        VideoFramePixelLayout::Yuv444Planar8 => Ok(ExpectedHostPlanarLayout {
            chroma_width: coded_width,
            chroma_height: coded_height,
            bytes_per_sample: 1,
        }),
        VideoFramePixelLayout::Yuv444Planar10Le => Ok(ExpectedHostPlanarLayout {
            chroma_width: coded_width,
            chroma_height: coded_height,
            bytes_per_sample: 2,
        }),
        _ => bail!("{pixel_layout} is not an accepted host-planar layout"),
    }
}

const fn half_rounded_up(dimension: u32) -> u32 {
    (dimension / 2) + (dimension % 2)
}

fn validate_host_plane_metadata(
    pixel_layout: VideoFramePixelLayout,
    plane: &HostPlaneDescriptor,
    expected_plane: &ExpectedHostPlane,
) -> anyhow::Result<()> {
    ensure!(
        plane.role == expected_plane.role,
        "host-planar role mismatch for {pixel_layout}: expected {:?}, got {:?}",
        expected_plane.role,
        plane.role
    );
    ensure!(
        plane.visible_width == expected_plane.visible_width
            && plane.visible_height == expected_plane.visible_height,
        "host-planar {:?} visible size mismatch for {pixel_layout}: expected {}x{}, got {}x{}",
        plane.role,
        expected_plane.visible_width,
        expected_plane.visible_height,
        plane.visible_width,
        plane.visible_height
    );
    ensure!(
        plane.bytes_per_sample == expected_plane.bytes_per_sample,
        "host-planar {:?} bytes-per-sample mismatch for {pixel_layout}: expected {}, got {}",
        plane.role,
        expected_plane.bytes_per_sample,
        plane.bytes_per_sample
    );
    Ok(())
}

fn validate_host_plane_visible_bounds(
    descriptor: &HostPlanarFrameDescriptor,
    plane_index: usize,
    plane: &HostPlaneDescriptor,
) -> anyhow::Result<()> {
    let visible_row_bytes = host_plane_visible_row_bytes(plane)?;
    ensure!(
        plane.stride >= visible_row_bytes,
        "host-planar {:?} stride {} is smaller than visible row bytes {}",
        plane.role,
        plane.stride,
        visible_row_bytes
    );

    let last_visible_row = plane.visible_height.saturating_sub(1);
    descriptor.visible_plane_row_bytes(plane_index, last_visible_row)?;

    Ok(())
}
