//! Shell-level scan доступных системных capabilities.
//!
//! Модуль сохраняет текущую policy app composition root: VA-API provider плюс
//! render capabilities, полученные от активного renderer.

use capability_core::CapabilityScanner;
use render_core::RenderCapabilities;

/// Запускает compile-time зарегистрированные capability probes.
pub(crate) fn probe_system_capabilities(
    render_capabilities: RenderCapabilities,
) -> capability_core::SystemCapabilities {
    let mut scanner = CapabilityScanner::new();
    scanner.register_provider(Box::new(video_vaapi::VaapiCapabilityProvider::new()));
    scanner.register_provider(Box::new(
        video_ffmpeg::FfmpegSoftwareCapabilityProvider::new(),
    ));
    scanner.register_render_capabilities(render_capabilities);
    scanner.scan()
}
