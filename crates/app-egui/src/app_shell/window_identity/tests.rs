use std::path::{Path, PathBuf};

use desktop_integration::FASTIPLAYER_APPLICATION_ID;
use winit::window::Window;

use super::*;

/// Размеры иконок, которые ставит `scripts/install-desktop-entry.sh`.
const PACKAGED_ICON_SIZES: [u32; 9] = [16, 22, 24, 32, 48, 64, 128, 256, 512];

/// Каталог `packaging/linux/` в корне репозитория.
fn packaging_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux")
}

/// Путь к иконке заданного размера в теме hicolor.
fn packaged_icon_path(size: u32) -> PathBuf {
    packaging_dir()
        .join("icons/hicolor")
        .join(format!("{size}x{size}/apps"))
        .join(format!("{FASTIPLAYER_APPLICATION_ID}.png"))
}

/// Значение ключа из `.desktop`-файла.
fn desktop_entry_value<'entry>(entry_text: &'entry str, key: &str) -> Option<&'entry str> {
    entry_text
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

/// Кодирует маленький PNG заданного формата для проверки отказов декодера.
fn encode_png(color_type: png::ColorType, width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let mut png_bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
    encoder.set_color(color_type);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header encodes");
    writer.write_image_data(pixels).expect("PNG pixels encode");
    writer.finish().expect("PNG stream finishes");
    png_bytes
}

#[test]
fn embedded_icon_decodes_to_white_logo_with_dark_outline() {
    let decoded = decode_window_icon_png(EMBEDDED_WINDOW_ICON_PNG).expect("embedded icon decodes");

    assert_eq!((decoded.width, decoded.height), (256, 256));
    assert_eq!(decoded.rgba_pixels.len(), 256 * 256 * RGBA_BYTES_PER_PIXEL);

    let opaque_pixels = || {
        decoded
            .rgba_pixels
            .as_chunks::<RGBA_BYTES_PER_PIXEL>()
            .0
            .iter()
            .filter(|pixel| pixel[3] == u8::MAX)
    };
    // Белое тело лого и тёмный контур, который делает его видимым на светлой панели.
    let white_pixel_count = opaque_pixels()
        .filter(|pixel| pixel[..3].iter().all(|&channel| channel > 240))
        .count();
    let dark_outline_pixel_count = opaque_pixels()
        .filter(|pixel| pixel[..3].iter().all(|&channel| channel < 48))
        .count();
    assert!(
        white_pixel_count > 5_000,
        "white logo body: {white_pixel_count}"
    );
    assert!(
        dark_outline_pixel_count > 2_000,
        "dark outline: {dark_outline_pixel_count}"
    );

    // Углы прозрачные: иконка — силуэт, а не квадратная плашка.
    assert_eq!(
        decoded.rgba_pixels[3], 0,
        "top-left corner must be transparent"
    );
}

#[test]
fn window_identity_attaches_icon_to_window_attributes() {
    let attributes = with_window_identity(Window::default_attributes());

    assert!(attributes.window_icon.is_some());
}

#[test]
fn window_identity_keeps_attributes_it_does_not_own() {
    let attributes = with_window_identity(Window::default_attributes().with_title("Probe"));

    assert_eq!(attributes.title, "Probe");
    assert!(
        attributes.decorations,
        "identity must not touch decorations"
    );
}

#[test]
fn corrupted_png_is_reported_as_decode_error() {
    let error = decode_window_icon_png(b"definitely not a png").expect_err("garbage is rejected");

    assert!(matches!(error, WindowIconError::Decode(_)), "{error:?}");
}

#[test]
fn non_rgba_png_is_rejected_instead_of_recolored() {
    let grayscale_png = encode_png(png::ColorType::Grayscale, 2, 2, &[0, 255, 255, 0]);

    let error = decode_window_icon_png(&grayscale_png).expect_err("grayscale is rejected");

    assert!(
        matches!(
            error,
            WindowIconError::UnsupportedPixelFormat {
                color_type: png::ColorType::Grayscale,
                bit_depth: png::BitDepth::Eight,
            }
        ),
        "{error:?}"
    );
}

#[test]
fn small_rgba_png_keeps_exact_pixels() {
    let rgba_pixels = [255, 0, 0, 255, 0, 0, 255, 128];
    let rgba_png = encode_png(png::ColorType::Rgba, 2, 1, &rgba_pixels);

    let decoded = decode_window_icon_png(&rgba_png).expect("RGBA decodes");

    assert_eq!((decoded.width, decoded.height), (2, 1));
    assert_eq!(decoded.rgba_pixels, rgba_pixels);
}

#[test]
fn embedded_icon_matches_packaged_hicolor_icon() {
    let packaged_icon = std::fs::read(packaged_icon_path(256)).expect("packaged 256px icon exists");

    assert_eq!(
        packaged_icon, EMBEDDED_WINDOW_ICON_PNG,
        "copy packaging/linux/icons/hicolor/256x256 icon to crates/app-egui/assets"
    );
}

#[test]
fn every_packaged_icon_size_is_square_rgba_of_its_size() {
    for size in PACKAGED_ICON_SIZES {
        let png_bytes = std::fs::read(packaged_icon_path(size))
            .unwrap_or_else(|error| panic!("{size}px icon must exist: {error}"));

        let decoded = decode_window_icon_png(&png_bytes)
            .unwrap_or_else(|error| panic!("{size}px icon must decode: {error}"));

        assert_eq!((decoded.width, decoded.height), (size, size));
    }
}

#[test]
fn desktop_entry_is_bound_to_window_application_id() {
    let desktop_file = packaging_dir().join(format!("{FASTIPLAYER_APPLICATION_ID}.desktop"));
    let entry_text = std::fs::read_to_string(&desktop_file).expect(".desktop file exists");

    // Wayland ищет .desktop по имени файла = app_id, X11 — по StartupWMClass = класс окна.
    assert_eq!(
        desktop_entry_value(&entry_text, "Icon"),
        Some(FASTIPLAYER_APPLICATION_ID)
    );
    assert_eq!(
        desktop_entry_value(&entry_text, "StartupWMClass"),
        Some(FASTIPLAYER_APPLICATION_ID)
    );
    assert_eq!(
        desktop_entry_value(&entry_text, "Exec"),
        Some("fastiplayer %U")
    );
    assert_eq!(
        desktop_entry_value(&entry_text, "Type"),
        Some("Application")
    );
}
