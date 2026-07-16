//! Общий mapping codec adapter failures в player error model.

use codec_core::VideoRequirementRejection;

use crate::{PlayerError, PlayerErrorKind};

/// Переводит codec adapter reject без generic hardware wording.
pub(super) fn player_error_from_requirement_rejection(
    rejection: VideoRequirementRejection,
) -> PlayerError {
    let kind = match rejection {
        VideoRequirementRejection::UnsupportedBitDepth { .. } => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        VideoRequirementRejection::UnsupportedChroma { .. }
        | VideoRequirementRejection::UnsupportedChromaFormat { .. } => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        VideoRequirementRejection::UnsupportedProfile { .. } => {
            PlayerErrorKind::UnsupportedVideoProfile
        }
        VideoRequirementRejection::UnsupportedPacketization { .. }
        | VideoRequirementRejection::UnsupportedCodecAdapter { .. } => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
    };

    PlayerError::new(kind, rejection.user_message())
}
