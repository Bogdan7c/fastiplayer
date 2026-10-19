//! Опознавательные знаки главного окна для рабочего стола: идентификатор приложения и иконка.
//!
//! Откуда панель задач берёт иконку окна, зависит от оконной системы:
//! - Wayland (KDE, GNOME): только из `.desktop`-файла с именем, равным app_id окна.
//!   Передать картинку из программы winit 0.30 на Wayland не умеет, поэтому здесь
//!   важно лишь задать правильный app_id, а сами `.desktop` и иконки ставит
//!   `scripts/install-desktop-entry.sh` из `packaging/linux/`.
//! - X11: картинку передаёт сама программа (свойство `_NET_WM_ICON`), а класс окна
//!   (`WM_CLASS`) связывает его с тем же `.desktop`-файлом через `StartupWMClass`.
//!
//! Модуль только дополняет атрибуты окна; создание окна и его lifecycle остаются у
//! [`super::AppShell`].

use std::io::Cursor;

use thiserror::Error;
use winit::window::{BadIcon, Icon, WindowAttributes};

#[cfg(target_os = "linux")]
use desktop_integration::FASTIPLAYER_APPLICATION_ID;

/// Иконка окна для X11: копия `packaging/linux/icons/hicolor/256x256/apps/…png`.
///
/// Копия лежит внутри crate-а, чтобы сборка не зависела от файлов вне его каталога;
/// тест `embedded_icon_matches_packaged_hicolor_icon` не даёт копиям разойтись.
/// 256×256 — компромисс: исходник 1254×1254 весил бы ~6 МБ в свойстве окна X11.
const EMBEDDED_WINDOW_ICON_PNG: &[u8] = include_bytes!("../../assets/window-icon-256.png");

/// Instance-часть `WM_CLASS` на X11: по соглашению это имя исполняемого файла.
#[cfg(target_os = "linux")]
const X11_WINDOW_INSTANCE_NAME: &str = "fastiplayer";

/// Число байтов на один пиксель RGBA 8 бит: красный, зелёный, синий, прозрачность.
const RGBA_BYTES_PER_PIXEL: usize = 4;

/// Почему встроенную иконку не удалось превратить в иконку окна.
#[derive(Debug, Error)]
pub(super) enum WindowIconError {
    /// PNG повреждён или не читается.
    #[error("window icon PNG is unreadable: {0}")]
    Decode(#[from] png::DecodingError),

    /// PNG читается, но хранит пиксели не как RGBA 8 бит, которые ждёт winit.
    #[error("window icon PNG must be 8-bit RGBA, got {color_type:?} at {bit_depth:?}")]
    UnsupportedPixelFormat {
        color_type: png::ColorType,
        bit_depth: png::BitDepth,
    },

    /// winit отверг пиксели (размер буфера не совпал с шириной и высотой).
    #[error("winit rejected window icon pixels: {0}")]
    Rejected(#[from] BadIcon),
}

/// Пиксели иконки, готовые для [`Icon::from_rgba`].
#[derive(Debug)]
pub(super) struct DecodedWindowIcon {
    /// Пиксели построчно, по [`RGBA_BYTES_PER_PIXEL`] байта на пиксель.
    pub(super) rgba_pixels: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
}

/// Добавляет к атрибутам окна идентификатор приложения и иконку.
///
/// Испорченная иконка не мешает запуску: окно создаётся со стандартным значком,
/// а причина пишется в лог предупреждением.
#[must_use]
pub(super) fn with_window_identity(attributes: WindowAttributes) -> WindowAttributes {
    let attributes = with_application_name(attributes);
    match load_embedded_window_icon() {
        Ok(icon) => attributes.with_window_icon(Some(icon)),
        Err(error) => {
            tracing::warn!(%error, "Window icon is unavailable, using the system default");
            attributes
        }
    }
}

/// Задаёт app_id (Wayland) и класс окна (X11) — по ним рабочий стол находит `.desktop`.
#[cfg(target_os = "linux")]
fn with_application_name(attributes: WindowAttributes) -> WindowAttributes {
    // Трейты Wayland и X11 пишут одно и то же поле winit, поэтому хватает одного:
    // на Wayland `general` становится app_id, на X11 — классом в `WM_CLASS`.
    use winit::platform::wayland::WindowAttributesExtWayland;
    attributes.with_name(FASTIPLAYER_APPLICATION_ID, X11_WINDOW_INSTANCE_NAME)
}

/// На других ОС связь окна с приложением устроена иначе; здесь атрибуты не меняются.
#[cfg(not(target_os = "linux"))]
fn with_application_name(attributes: WindowAttributes) -> WindowAttributes {
    attributes
}

/// Превращает встроенный PNG в иконку winit.
fn load_embedded_window_icon() -> Result<Icon, WindowIconError> {
    let decoded = decode_window_icon_png(EMBEDDED_WINDOW_ICON_PNG)?;
    Ok(Icon::from_rgba(
        decoded.rgba_pixels,
        decoded.width,
        decoded.height,
    )?)
}

/// Читает PNG и проверяет, что пиксели уже в RGBA 8 бит.
///
/// Преобразований цвета здесь нет намеренно: иконка готовится заранее
/// (`packaging/linux/`), и неожиданный формат — ошибка сборки ассета, а не повод
/// молча перекрашивать картинку.
pub(super) fn decode_window_icon_png(
    png_bytes: &[u8],
) -> Result<DecodedWindowIcon, WindowIconError> {
    let mut reader = png::Decoder::new(Cursor::new(png_bytes)).read_info()?;
    // `None` бывает только при переполнении размера; пустой буфер тогда превращается
    // в ошибку `next_frame`, так что этот случай не теряется молча.
    let mut rgba_pixels = vec![0; reader.output_buffer_size().unwrap_or_default()];
    let frame = reader.next_frame(&mut rgba_pixels)?;

    if frame.color_type != png::ColorType::Rgba || frame.bit_depth != png::BitDepth::Eight {
        return Err(WindowIconError::UnsupportedPixelFormat {
            color_type: frame.color_type,
            bit_depth: frame.bit_depth,
        });
    }

    // Буфер декодера может быть больше одного кадра; лишний хвост отрезаем.
    rgba_pixels.truncate(frame.buffer_size());
    debug_assert_eq!(
        rgba_pixels.len(),
        frame.width as usize * frame.height as usize * RGBA_BYTES_PER_PIXEL
    );
    Ok(DecodedWindowIcon {
        rgba_pixels,
        width: frame.width,
        height: frame.height,
    })
}

#[cfg(test)]
mod tests;
