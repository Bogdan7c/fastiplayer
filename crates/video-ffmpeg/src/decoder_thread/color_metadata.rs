use codec_core::{
    ColorPrimaries, ColorRange, HdrMetadata, MatrixCoefficients, TransferFunction,
    VideoColorMetadata,
};

#[cfg(any(test, feature = "ffmpeg"))]
use super::send_receive::ReceivedFrameMetadata;

#[cfg(any(test, feature = "ffmpeg"))]
pub(super) fn frame_color_or_context_color(
    frame_metadata: &ReceivedFrameMetadata,
    context_color: &Option<VideoColorMetadata>,
) -> Option<VideoColorMetadata> {
    #[cfg(feature = "ffmpeg")]
    {
        merge_frame_color_with_context_color(frame_metadata.color.clone(), context_color)
    }

    #[cfg(not(feature = "ffmpeg"))]
    {
        let _frame_metadata = frame_metadata;
        context_color.as_ref().cloned()
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
pub(super) fn merge_frame_color_with_context_color(
    frame_color: Option<VideoColorMetadata>,
    context_color: &Option<VideoColorMetadata>,
) -> Option<VideoColorMetadata> {
    match (frame_color, context_color.as_ref()) {
        (Some(frame_color), Some(context_color)) if color_core_is_unknown(&frame_color) => {
            let mut merged_color = context_color.clone();
            merged_color.hdr_metadata =
                align_hdr_metadata_to_color(frame_color.hdr_metadata, &merged_color)
                    .or(merged_color.hdr_metadata);
            Some(merged_color)
        }
        (Some(mut frame_color), Some(context_color)) => {
            fill_unknown_core_color_fields(&mut frame_color, context_color);
            let frame_hdr_metadata = frame_color.hdr_metadata.take();
            frame_color.hdr_metadata =
                align_hdr_metadata_to_color(frame_hdr_metadata, &frame_color)
                    .or_else(|| context_color.hdr_metadata.clone());
            Some(frame_color)
        }
        (Some(frame_color), None) => Some(frame_color),
        (None, Some(context_color)) => Some(context_color.clone()),
        (None, None) => None,
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
fn color_core_is_unknown(color: &VideoColorMetadata) -> bool {
    color.range == ColorRange::Unknown
        && color.matrix == MatrixCoefficients::Unknown
        && color.primaries == ColorPrimaries::Unknown
        && color.transfer == TransferFunction::Unknown
}

#[cfg(any(test, feature = "ffmpeg"))]
fn fill_unknown_core_color_fields(
    frame_color: &mut VideoColorMetadata,
    context_color: &VideoColorMetadata,
) {
    if frame_color.range == ColorRange::Unknown {
        frame_color.range = context_color.range;
    }
    if frame_color.matrix == MatrixCoefficients::Unknown {
        frame_color.matrix = context_color.matrix;
    }
    if frame_color.primaries == ColorPrimaries::Unknown {
        frame_color.primaries = context_color.primaries;
    }
    if frame_color.transfer == TransferFunction::Unknown {
        frame_color.transfer = context_color.transfer;
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
fn align_hdr_metadata_to_color(
    hdr_metadata: Option<HdrMetadata>,
    color: &VideoColorMetadata,
) -> Option<HdrMetadata> {
    hdr_metadata.map(|mut hdr_metadata| {
        if hdr_metadata.color_primaries == ColorPrimaries::Unknown {
            hdr_metadata.color_primaries = color.primaries;
        }
        if hdr_metadata.transfer_function == TransferFunction::Unknown {
            hdr_metadata.transfer_function = color.transfer;
        }
        hdr_metadata
    })
}
