//! UTF-8 Extended M3U8 serializer.

use std::fmt::Write;

use super::{PreparedPlaylistExport, export_title, sanitize_m3u8_text};

/// Сериализует checked canonical track list без group metadata guessing.
pub(super) fn serialize(plan: &PreparedPlaylistExport) -> String {
    let mut output = String::from("#EXTM3U\n");
    for track in &plan.tracks {
        output.push_str("#EXTINF:");
        push_duration(&mut output, track.metadata.duration());
        output.push(',');
        output.push_str(&sanitize_m3u8_text(export_title(&track.metadata)));
        output.push('\n');
        output.push_str(track.locator.as_str());
        output.push('\n');
    }
    output
}

/// M3U8 decimal seconds сохраняют nanosecond precision `MediaDuration`.
fn push_duration(output: &mut String, duration: Option<media_core::MediaDuration>) {
    let Some(duration) = duration else {
        output.push_str("-1");
        return;
    };
    let duration = duration.as_duration();
    let seconds = duration.as_secs();
    let nanoseconds = duration.subsec_nanos();
    write!(output, "{seconds}").expect("String formatting infallible");
    if nanoseconds == 0 {
        return;
    }
    let fractional = format!("{nanoseconds:09}");
    output.push('.');
    output.push_str(fractional.trim_end_matches('0'));
}
