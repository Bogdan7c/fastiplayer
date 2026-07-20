//! XSPF v1 serializer с playlist-level Rustiplayer group extension.

use std::fmt::Write;

use crate::{RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE, XSPF_NAMESPACE};

use super::{
    PreparedPlaylistExport, export_title, push_xml_attribute, push_xml_text,
    xspf_duration_milliseconds, xspf_track_number,
};

/// Сериализует checked plan в deterministic namespace-aware XSPF v1.
pub(super) fn serialize(plan: &PreparedPlaylistExport) -> String {
    let mut output = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    writeln!(
        output,
        "<playlist version=\"1\" xmlns=\"{XSPF_NAMESPACE}\" xmlns:rp=\"{RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE}\">"
    )
    .expect("String formatting infallible");
    push_group_extension(&mut output, plan);
    output.push_str("  <trackList>\n");
    for track in &plan.tracks {
        output.push_str("    <track>\n");
        output.push_str("      <location>");
        push_xml_text(&mut output, track.locator.as_str());
        output.push_str("</location>\n");
        output.push_str("      <title>");
        push_xml_text(&mut output, export_title(&track.metadata));
        output.push_str("</title>\n");
        if let Some(creator) = track.metadata.artists().first() {
            output.push_str("      <creator>");
            push_xml_text(&mut output, creator);
            output.push_str("</creator>\n");
        }
        if let Some(album) = track.metadata.album() {
            output.push_str("      <album>");
            push_xml_text(&mut output, album);
            output.push_str("</album>\n");
        }
        if let Some(track_number) = xspf_track_number(&track.metadata) {
            writeln!(output, "      <trackNum>{track_number}</trackNum>")
                .expect("String formatting infallible");
        }
        if let Some(duration_milliseconds) = xspf_duration_milliseconds(&track.metadata) {
            writeln!(output, "      <duration>{duration_milliseconds}</duration>")
                .expect("String formatting infallible");
        }
        output.push_str("    </track>\n");
    }
    output.push_str("  </trackList>\n");
    output.push_str("</playlist>\n");
    output
}

/// Публикует один known extension container до required trackList.
fn push_group_extension(output: &mut String, plan: &PreparedPlaylistExport) {
    if plan.groups.is_empty() {
        return;
    }
    output.push_str("  <extension application=\"");
    push_xml_attribute(output, RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE);
    output.push_str("\">\n");
    for group in &plan.groups {
        writeln!(
            output,
            "    <rp:group firstTrack=\"{}\" trackCount=\"{}\">",
            group.first_track, group.track_count
        )
        .expect("String formatting infallible");
        output.push_str("      <rp:location>");
        push_xml_text(output, group.root_locator.as_str());
        output.push_str("</rp:location>\n");
        output.push_str("    </rp:group>\n");
    }
    output.push_str("  </extension>\n");
}
