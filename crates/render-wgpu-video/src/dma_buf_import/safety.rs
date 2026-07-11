//! Safety primitives Vulkan DMA-BUF import boundary.

use std::os::fd::{BorrowedFd, OwnedFd, RawFd};

use anyhow::Context;
use ash::vk;

/// Дублирует DMA-BUF fd перед передачей во Vulkan import.
///
/// Исходный `source_fd` принадлежит нейтральному descriptor/provider, поэтому
/// Vulkan получает отдельный duplicate с `CLOEXEC`.
pub(super) fn duplicate_fd_for_vulkan_import(
    source_fd: RawFd,
    import_context: &'static str,
) -> anyhow::Result<OwnedFd> {
    // SAFETY: `source_fd` заимствован у живого `DmaBufObjectDescriptor` на
    // время синхронного клонирования. `BorrowedFd` не закрывает исходный fd.
    let borrowed_source = unsafe { BorrowedFd::borrow_raw(source_fd) };
    borrowed_source
        .try_clone_to_owned()
        .with_context(|| format!("{import_context}: dup dma-buf fd for Vulkan import failed"))
}

/// Выбирает memory type из пересечения требований image и импортируемого fd.
///
/// Vulkan требует учитывать `vkGetMemoryFdPropertiesKHR::memoryTypeBits`, а
/// `DEVICE_LOCAL` здесь только предпочтение: некоторые совместимые DMA-BUF
/// heaps не публикуют этот флаг.
pub(super) fn select_import_memory_type_index(
    image_memory_type_bits: u32,
    fd_memory_type_bits: u32,
    memory_types: &[vk::MemoryType],
) -> Option<u32> {
    let compatible_bits = image_memory_type_bits & fd_memory_type_bits;
    let compatible = memory_types
        .iter()
        .enumerate()
        .filter(|(index, memory_type)| {
            let bit = 1_u32.checked_shl(*index as u32).unwrap_or(0);
            compatible_bits & bit != 0
                && !memory_type
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::PROTECTED)
        });

    compatible
        .clone()
        .find(|(_, memory_type)| {
            memory_type
                .property_flags
                .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        })
        .or_else(|| compatible.into_iter().next())
        .and_then(|(index, _)| u32::try_from(index).ok())
}

/// Запрашивает memory types, с которыми Vulkan разрешает импортировать fd.
pub(super) fn query_dma_buf_memory_type_bits(
    raw_instance: &ash::Instance,
    raw_device: &ash::Device,
    source_fd: RawFd,
    import_context: &'static str,
) -> anyhow::Result<u32> {
    let external_memory_fd = ash::khr::external_memory_fd::Device::new(raw_instance, raw_device);
    let mut fd_properties = vk::MemoryFdPropertiesKHR::default();

    // SAFETY: extension loader построен из совместимых instance/device одного
    // WGPU Vulkan context-а. Query не импортирует и не закрывает `source_fd`.
    unsafe {
        external_memory_fd.get_memory_fd_properties(
            vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
            source_fd,
            &mut fd_properties,
        )
    }
    .with_context(|| format!("{import_context}: vkGetMemoryFdPropertiesKHR failed"))?;

    Ok(fd_properties.memory_type_bits)
}

/// Сверяет число передаваемых layouts с числом memory planes DRM modifier-а.
fn validate_modifier_plane_count(
    provided_plane_count: u32,
    modifier_plane_count: u32,
    modifier: u64,
    import_context: &'static str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        provided_plane_count == modifier_plane_count,
        "{import_context}: DRM modifier {modifier:#x} requires {modifier_plane_count} plane layouts, descriptor provides {provided_plane_count}"
    );
    Ok(())
}

/// Запрашивает реальное число memory planes выбранного format/modifier pair.
pub(super) fn validate_drm_modifier_plane_count(
    raw_instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    format: vk::Format,
    modifier: u64,
    provided_plane_count: u32,
    import_context: &'static str,
) -> anyhow::Result<()> {
    let mut count_query = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut format_properties = vk::FormatProperties2::default().push_next(&mut count_query);

    // SAFETY: `physical_device` принадлежит `raw_instance`; pNext указывает на
    // живой output, а вызов только читает immutable format properties.
    unsafe {
        raw_instance.get_physical_device_format_properties2(
            physical_device,
            format,
            &mut format_properties,
        )
    };

    let property_count = usize::try_from(count_query.drm_format_modifier_count)
        .context("DRM modifier property count does not fit usize")?;
    let mut modifier_properties =
        vec![vk::DrmFormatModifierPropertiesEXT::default(); property_count];
    let mut list_query = vk::DrmFormatModifierPropertiesListEXT::default()
        .drm_format_modifier_properties(&mut modifier_properties);
    let mut format_properties = vk::FormatProperties2::default().push_next(&mut list_query);

    // SAFETY: output slice жив и имеет capacity, полученную первым Vulkan
    // query для тех же immutable format properties.
    unsafe {
        raw_instance.get_physical_device_format_properties2(
            physical_device,
            format,
            &mut format_properties,
        )
    };

    let returned_property_count = usize::try_from(list_query.drm_format_modifier_count)
        .context("returned DRM modifier property count does not fit usize")?;
    anyhow::ensure!(
        returned_property_count <= modifier_properties.len(),
        "DRM modifier property count grew between Vulkan queries"
    );
    let modifier_plane_count = modifier_properties[..returned_property_count]
        .iter()
        .find(|properties| properties.drm_format_modifier == modifier)
        .map(|properties| properties.drm_format_modifier_plane_count)
        .with_context(|| {
            format!(
                "{import_context}: DRM modifier {modifier:#x} is not supported for Vulkan format {format:?}"
            )
        })?;

    validate_modifier_plane_count(
        provided_plane_count,
        modifier_plane_count,
        modifier,
        import_context,
    )
}

/// Проверяет, что видимый payload plane помещается в DMA-BUF object.
pub(super) fn validate_plane_bounds(
    object_size: u64,
    offset: u64,
    row_pitch: u32,
    width: u32,
    height: u32,
    bytes_per_texel: u32,
    plane_context: &'static str,
) -> anyhow::Result<()> {
    anyhow::ensure!(width > 0 && height > 0, "{plane_context} has zero extent");

    let visible_row_bytes = u64::from(width)
        .checked_mul(u64::from(bytes_per_texel))
        .with_context(|| format!("{plane_context} visible row size overflows"))?;
    anyhow::ensure!(
        u64::from(row_pitch) >= visible_row_bytes,
        "{plane_context} row pitch {row_pitch} is smaller than visible row size {visible_row_bytes}"
    );

    let preceding_rows_size = u64::from(row_pitch)
        .checked_mul(u64::from(height - 1))
        .with_context(|| format!("{plane_context} row span overflows"))?;
    let payload_end = offset
        .checked_add(preceding_rows_size)
        .and_then(|end| end.checked_add(visible_row_bytes))
        .with_context(|| format!("{plane_context} payload end overflows"))?;
    anyhow::ensure!(
        payload_end <= object_size,
        "{plane_context} payload end {payload_end} exceeds DMA-BUF object size {object_size}"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn vulkan_import_fd_duplicate_is_cloexec_and_does_not_close_source() {
        use nix::fcntl::{FcntlArg, FdFlag, fcntl};
        use std::fs::File;

        let source = File::open("/dev/null").expect("test source fd must open");
        let duplicate = duplicate_fd_for_vulkan_import(source.as_raw_fd(), "test ownership")
            .expect("source fd must be duplicated");
        let flags = fcntl(duplicate.as_raw_fd(), FcntlArg::F_GETFD)
            .map(FdFlag::from_bits_truncate)
            .expect("duplicate fd flags must be readable");
        assert!(flags.contains(FdFlag::FD_CLOEXEC));

        drop(duplicate);
        fcntl(source.as_raw_fd(), FcntlArg::F_GETFD)
            .expect("dropping duplicate must not close source fd");
    }

    #[test]
    fn imported_memory_type_uses_image_and_fd_intersection() {
        let memory_types = [
            vk::MemoryType::default().property_flags(vk::MemoryPropertyFlags::DEVICE_LOCAL),
            vk::MemoryType::default().property_flags(vk::MemoryPropertyFlags::HOST_VISIBLE),
        ];
        assert_eq!(
            select_import_memory_type_index(0b11, 0b10, &memory_types),
            Some(1)
        );
    }

    #[test]
    fn imported_memory_type_prefers_device_local() {
        let memory_types = [
            vk::MemoryType::default().property_flags(vk::MemoryPropertyFlags::HOST_VISIBLE),
            vk::MemoryType::default().property_flags(vk::MemoryPropertyFlags::DEVICE_LOCAL),
        ];
        assert_eq!(
            select_import_memory_type_index(0b11, 0b11, &memory_types),
            Some(1)
        );
    }

    #[test]
    fn imported_memory_type_rejects_incompatible_or_protected_types() {
        let ordinary_types = [vk::MemoryType::default(); 2];
        assert_eq!(
            select_import_memory_type_index(0b01, 0b10, &ordinary_types),
            None
        );

        let protected_type =
            [vk::MemoryType::default().property_flags(vk::MemoryPropertyFlags::PROTECTED)];
        assert_eq!(
            select_import_memory_type_index(0b1, 0b1, &protected_type),
            None
        );
    }

    #[test]
    fn plane_bounds_reject_out_of_object_and_overflow() {
        let outside = validate_plane_bounds(4_096, 3_900, 256, 128, 2, 1, "test luma")
            .expect_err("out-of-object plane must be rejected");
        assert!(outside.to_string().contains("exceeds DMA-BUF object size"));

        let overflow =
            validate_plane_bounds(u64::MAX, u64::MAX - 1, u32::MAX, 1, 2, 4, "test chroma")
                .expect_err("overflowing layout must be rejected");
        assert!(overflow.to_string().contains("overflows"));
    }

    #[test]
    fn drm_modifier_rejects_unrepresented_memory_plane() {
        let error = validate_modifier_plane_count(2, 3, 0x0102_0304, "test import")
            .expect_err("modifier plane count mismatch must be rejected");
        assert!(error.to_string().contains("requires 3 plane layouts"));
    }
}
