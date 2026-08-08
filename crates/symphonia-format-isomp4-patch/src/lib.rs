// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]

mod atoms;
mod demuxer;
mod fp;
// Реконструкция fragment/init остаётся внутри единственного ISO-BMFF owner-а.
#[allow(dead_code)]
mod fragment_reconstruction;
mod stream;

pub use demuxer::{IsoMp4PacketWithSourceOffset, IsoMp4Reader};
pub use fragment_reconstruction::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacLcConfiguration,
    FragmentAacSampleRate, FragmentArithmeticOperation, FragmentBaseDecodeTime, FragmentBoxKind,
    FragmentBoxType, FragmentCodecConfigurationIssue, FragmentCodecKind, FragmentCodedCoverage,
    FragmentDrmEvidence, FragmentH264Configuration, FragmentH264PictureParameterSet,
    FragmentH264SequenceParameterSet, FragmentInitializationCodec, FragmentInitializationError,
    FragmentInitializationField, FragmentInitializationLimitBuildError,
    FragmentInitializationLimitKind, FragmentInitializationLimits,
    FragmentInitializationLimitsBuilder, FragmentInitializationRequest,
    FragmentInitializationSegment, FragmentInspectionError, FragmentInspectionLimitBuildError,
    FragmentInspectionLimitKind, FragmentInspectionLimits, FragmentInspectionLimitsBuilder,
    FragmentMediaBoxType, FragmentMediaKind, FragmentPrivateExtension, FragmentReconstructionError,
    FragmentReconstructionRequest, FragmentSampleDefaults, FragmentStructureContext,
    FragmentTimescale, FragmentTimingEvidence, FragmentTrackId, FragmentTrackReconstructionIntent,
    FragmentUnsupportedLayout, FragmentVideoDimensions, FragmentVideoHeight, FragmentVideoWidth,
    FragmentWriteArithmeticOperation, FragmentWriteCancellationPhase, FragmentWriteError,
    FragmentWriteLimitBuildError, FragmentWriteLimits, ReconstructedMediaSegment,
    build_fragmented_initialization_segment, reconstruct_media_fragment,
};
