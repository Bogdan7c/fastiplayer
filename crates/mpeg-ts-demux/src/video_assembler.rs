use std::collections::HashMap;

use crate::MpegTsDemuxError;
use crate::elementary::{
    ElementaryPacket, classify_video_access_unit, video_access_unit_boundaries,
};

/// Полностью собранный AU вместе с timestamp evidence PES, в котором он начался.
pub(crate) struct TimedVideoAccessUnit {
    pub(crate) packet: ElementaryPacket,
    pub(crate) pts: i64,
    pub(crate) dts: Option<i64>,
    pub(crate) byte_offset: u64,
}

/// Stateful bounded assembler одного elementary video PID.
#[derive(Clone)]
struct VideoAccessUnitState {
    bytes: Vec<u8>,
    pts: i64,
    dts: Option<i64>,
    byte_offset: u64,
    is_h265: bool,
}

/// Владелец per-PID AU buffers; PES boundaries здесь считаются только chunks.
#[derive(Clone)]
pub(crate) struct VideoAccessUnitAssembler {
    states: HashMap<u16, VideoAccessUnitState>,
    limit_bytes: usize,
}

impl VideoAccessUnitAssembler {
    pub(crate) fn new(limit_bytes: usize) -> Self {
        Self {
            states: HashMap::new(),
            limit_bytes,
        }
    }

    /// Добавляет PES payload и отдаёт только AU, завершённые следующей AU boundary.
    pub(crate) fn push(
        &mut self,
        pid: u16,
        payload: &[u8],
        pts: i64,
        dts: Option<i64>,
        byte_offset: u64,
        is_h265: bool,
    ) -> Result<Vec<TimedVideoAccessUnit>, MpegTsDemuxError> {
        let state = self
            .states
            .entry(pid)
            .or_insert_with(|| VideoAccessUnitState {
                bytes: Vec::new(),
                pts,
                dts,
                byte_offset,
                is_h265,
            });
        if state.is_h265 != is_h265 {
            return Err(MpegTsDemuxError::Malformed {
                reason: format!("video codec изменился без topology reset на PID {pid}"),
            });
        }
        if state.bytes.len().saturating_add(payload.len()) > self.limit_bytes {
            return Err(MpegTsDemuxError::VideoAccessUnitTooLarge {
                pid,
                limit_bytes: self.limit_bytes,
            });
        }
        state.bytes.extend_from_slice(payload);
        let ranges = video_access_unit_boundaries(&state.bytes, is_h265)?;
        let Some((next_start, _)) = ranges.last().copied().filter(|_| ranges.len() > 1) else {
            return Ok(Vec::new());
        };

        let completed_bytes: Vec<u8> = state.bytes.drain(..next_start).collect();
        let completed_ranges = video_access_unit_boundaries(&completed_bytes, is_h265)?;
        let completed = completed_ranges
            .into_iter()
            .map(|(start, end)| {
                classify_video_access_unit(&completed_bytes[start..end], is_h265).map(|packet| {
                    TimedVideoAccessUnit {
                        packet,
                        pts: state.pts,
                        dts: state.dts,
                        byte_offset: state.byte_offset,
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        state.pts = pts;
        state.dts = dts;
        state.byte_offset = byte_offset;
        Ok(completed)
    }

    /// EOF доказывает конец последнего AU; пустой state ничего не публикует.
    pub(crate) fn finish_pid(
        &mut self,
        pid: u16,
    ) -> Result<Option<TimedVideoAccessUnit>, MpegTsDemuxError> {
        let Some(state) = self.states.remove(&pid) else {
            return Ok(None);
        };
        if state.bytes.is_empty() {
            return Ok(None);
        }
        let packet = classify_video_access_unit(&state.bytes, state.is_h265)?;
        Ok(Some(TimedVideoAccessUnit {
            packet,
            pts: state.pts,
            dts: state.dts,
            byte_offset: state.byte_offset,
        }))
    }

    /// Continuity/TEI/discontinuity сбрасывает только повреждённый PID.
    pub(crate) fn reset_pid(&mut self, pid: u16) {
        self.states.remove(&pid);
    }

    /// Topology/seek reset удаляет все container-owned partial AU.
    pub(crate) fn reset_all(&mut self) {
        self.states.clear();
    }

    pub(crate) fn active_pids(&self) -> Vec<u16> {
        self.states.keys().copied().collect()
    }
}
