//! Read-only содержимое Info-панели из публичного player snapshot.

use egui::{RichText, Ui};
use player_core::PlayerSnapshot;

pub(crate) fn show(ui: &mut Ui, snapshot: &PlayerSnapshot) {
    let Some(info) = snapshot.media_info.as_ref() else {
        ui.label("No media file is open");
        return;
    };

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
