//! Read-only содержимое Info-панели из публичного player snapshot.

use egui::{RichText, Ui};
use player_core::PlayerSnapshot;

use crate::page_label::PageLabel;

/// Что показывать вместо заголовка у страницы без `<title>`.
const NO_TITLE_LABEL: &str = "(none)";

/// Строки блока `Page`: сайт и заголовок страницы-источника (04c).
fn page_rows(page: &PageLabel) -> [(&'static str, &str); 2] {
    let title = if page.title().trim().is_empty() {
        NO_TITLE_LABEL
    } else {
        page.title()
    };
    [("Site", page.domain()), ("Title", title)]
}

/// Блок `Page` в том же стиле, что заголовки секций дорожек. Домен и заголовок
/// живут только в памяти окна (инвариант 7).
fn show_page_block(ui: &mut Ui, page: &PageLabel) {
    ui.label(RichText::new("Page").strong());
    for (name, value) in page_rows(page) {
        row(ui, name, value);
    }
}

/// Рисует Info. `page` есть только у окна браузерного режима: блок `Page` идёт первым
/// и виден даже пока медиа ещё не открыто.
pub(crate) fn show(ui: &mut Ui, snapshot: &PlayerSnapshot, page: Option<&PageLabel>) {
    let has_page = page.is_some();
    if let Some(page) = page {
        show_page_block(ui, page);
    }
    let Some(info) = snapshot.media_info.as_ref() else {
        if has_page {
            ui.separator();
        }
        ui.label("No media file is open");
        return;
    };

    if has_page {
        ui.separator();
    }
    row(ui, "Source", &info.source.display_location);
    if let Some(size_bytes) = info.source.size_bytes {
        row(
            ui,
            "Size",
            &format!("{size_bytes} bytes ({})", format_iec_size(size_bytes)),
        );
    }
    if let Some(container) = info
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.container.as_ref())
        .and_then(|container| container.format_name.as_deref())
    {
        row(ui, "Container", container);
    }
    if let Some(duration) = info.duration {
        row(ui, "Duration", &format_duration(duration));
    }
    row(
        ui,
        "Seeking",
        if info.seekable {
            "available"
        } else {
            "unavailable"
        },
    );

    if let Some(tags) = info.metadata.as_ref().map(|metadata| &metadata.tags) {
        optional_row(ui, "Title", tags.title.as_deref());
        if !tags.artists.is_empty() {
            row(ui, "Artists", &tags.artists.join(", "));
        }
        optional_row(ui, "Album", tags.album.as_deref());
    }

    for track in &snapshot.tracks {
        ui.separator();
        let selected = snapshot.selected_tracks.video_track == Some(track.id)
            || snapshot.selected_tracks.audio_track == Some(track.id);
        ui.label(
            RichText::new(format!(
                "{:?} {}{}",
                track.kind,
                track.id.get(),
                if selected { " • selected" } else { "" }
            ))
            .strong(),
        );
        row(ui, "Codec", &track.codec_id);
        if let Some(duration) = track.duration {
            row(ui, "Track duration", &format_duration(duration));
        }
        if let Some(rate) = track.sample_rate {
            row(ui, "Sample rate", &format!("{rate} Hz"));
        }
        if let Some(channels) = track.channels {
            row(ui, "Channels", &channels.to_string());
        }
        if let Some(video) = track.video.as_ref() {
            if let (Some(width), Some(height)) = (video.coded_width, video.coded_height) {
                row(ui, "Resolution", &format!("{width}×{height}"));
            }
            optional_debug_row(ui, "Profile", video.profile.as_ref());
            optional_debug_row(ui, "Bit depth", video.bit_depth.as_ref());
            optional_debug_row(ui, "Chroma", video.chroma.as_ref());
            row(ui, "Orientation", &format!("{:?}", video.orientation));
            if let Some(color) = video.color.as_ref() {
                row(ui, "Color", &format!("{color:?}"));
            }
        }
    }
}

fn row(ui: &mut Ui, name: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("{name}:"));
        ui.add(egui::Label::new(value).wrap());
    });
}
fn optional_row(ui: &mut Ui, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        row(ui, name, value);
    }
}
fn optional_debug_row(ui: &mut Ui, name: &str, value: Option<&impl std::fmt::Debug>) {
    if let Some(value) = value {
        row(ui, name, &format!("{value:?}"));
    }
}

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn format_iec_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Плоский текст всех подписей, которые Info нарисовало за кадр.
    fn painted_texts(page: Option<&PageLabel>) -> Vec<String> {
        let snapshot = PlayerSnapshot::empty();
        let context = egui::Context::default();
        let output = crate::ui::test_frame::run_ui_frame(
            &context,
            crate::ui::test_frame::input_at(0.0, Vec::new()),
            |ui| show(ui, &snapshot, page),
        );
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    }

    /// Браузерное окно без медиа: Page первым, с доменом и заголовком, затем заглушка.
    #[test]
    fn page_block_leads_info_and_is_visible_without_media() {
        let texts = painted_texts(Some(&PageLabel::new("Cats", "video.example")));
        let position = |needle: &str| {
            texts
                .iter()
                .position(|text| text == needle)
                .unwrap_or_else(|| panic!("no `{needle}` in {texts:?}"))
        };
        assert_eq!(position("Page"), 0);
        assert!(position("Site:") < position("video.example"));
        assert!(position("video.example") < position("Title:"));
        assert!(position("Title:") < position("Cats"));
        assert!(position("Cats") < position("No media file is open"));
    }

    /// Страница без заголовка — понятная пометка.
    #[test]
    fn page_without_title_says_none() {
        let texts = painted_texts(Some(&PageLabel::new("  ", "video.example")));
        assert!(texts.contains(&"(none)".to_owned()), "{texts:?}");
    }

    /// Обычное окно: блока Page нет, остаётся только заглушка.
    #[test]
    fn standard_window_has_no_page_block() {
        assert_eq!(painted_texts(None), vec!["No media file is open"]);
    }

    #[test]
    fn iec_size_is_stable() {
        assert_eq!(format_iec_size(1_048_576), "1.0 MiB");
    }
    #[test]
    fn duration_is_zero_padded() {
        assert_eq!(
            format_duration(std::time::Duration::from_secs(3661)),
            "01:01:01"
        );
    }
}
