//! Решение о follow-up redraw после показа (cpu-parity, сессия 06a).

use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::*;

/// Текущее render generation снимка плеера в тестах.
const CURRENT_GENERATION: u64 = 4;

/// Кадр с заданным PTS (мс); handle уникален на PTS, как у настоящих кадров пула.
fn decoded_frame_at_ms(pts_ms: u64) -> DecodedFrame {
    DecodedFrame {
        generation: 1,
        pts: Duration::from_millis(pts_ms),
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 64,
        height: 36,
        render_width: 64,
        render_height: 36,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(pts_ms + 1),
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Identity кадра так, как его видит передача плеера.
fn slot_identity(render_generation: u64, pts_ms: u64) -> VideoPresentFrameIdentity {
    VideoPresentFrameIdentity::from_decoded_frame(render_generation, &decoded_frame_at_ms(pts_ms))
}

/// Identity кадра так, как его помнит кэш показанного кадра.
fn shown_identity(pts_ms: u64) -> PresentFrameIdentity {
    PresentFrameIdentity::from_decoded_frame(CURRENT_GENERATION, &decoded_frame_at_ms(pts_ms))
}

/// Worker выбрал следующий кадр, пока UI показывал предыдущий: он и есть ждущий.
#[test]
fn newer_frame_of_current_generation_needs_follow_up() {
    let waiting = slot_identity(CURRENT_GENERATION, 33);

    assert_eq!(
        waiting_present_frame_for_follow_up(
            PresentFrameIdentityProbe::Known(Some(waiting)),
            Some(shown_identity(16)),
            CURRENT_GENERATION,
        ),
        Some(waiting)
    );
    // Первый кадр после открытия: показанного ещё нет — ждущий кадр тоже нужно показать.
    assert_eq!(
        waiting_present_frame_for_follow_up(
            PresentFrameIdentityProbe::Known(Some(waiting)),
            None,
            CURRENT_GENERATION,
        ),
        Some(waiting)
    );
}

/// В передаче только что показанный кадр: ждать нечего, лишнего redraw нет.
#[test]
fn already_shown_frame_needs_no_follow_up() {
    assert_eq!(
        waiting_present_frame_for_follow_up(
            PresentFrameIdentityProbe::Known(Some(slot_identity(CURRENT_GENERATION, 16))),
            Some(shown_identity(16)),
            CURRENT_GENERATION,
        ),
        None
    );
}

/// Пустая передача, публикация в процессе (`Busy`) и кадр чужого поколения — без follow-up.
#[test]
fn empty_busy_or_foreign_generation_frame_needs_no_follow_up() {
    for probe in [
        PresentFrameIdentityProbe::Known(None),
        PresentFrameIdentityProbe::Busy,
        PresentFrameIdentityProbe::Known(Some(slot_identity(CURRENT_GENERATION + 1, 33))),
    ] {
        assert_eq!(
            waiting_present_frame_for_follow_up(
                probe,
                Some(shown_identity(16)),
                CURRENT_GENERATION
            ),
            None,
            "{probe:?}"
        );
    }
}

/// Память follow-up: для одного кадра запрос один, для следующего — снова запрос.
#[test]
fn follow_up_is_claimed_once_per_waiting_frame() {
    let mut memory = super::super::redraw_runtime::WaitingFrameFollowUpMemory::default();
    let first_waiting = slot_identity(CURRENT_GENERATION, 33);
    let second_waiting = slot_identity(CURRENT_GENERATION, 50);

    assert_eq!(
        memory.claim(first_waiting),
        WaitingPresentFrameFollowUp::Requested
    );
    assert_eq!(
        memory.claim(first_waiting),
        WaitingPresentFrameFollowUp::AlreadyRequested
    );
    assert_eq!(
        memory.claim(second_waiting),
        WaitingPresentFrameFollowUp::Requested
    );
}
