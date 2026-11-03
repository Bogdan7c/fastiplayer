//! Краткая выжимка строки сводки воспроизведения для отчёта: вид, кадры, позиция, состояние.

use super::player_log::{number_field, word_field};

/// Поля сводки, которые печатает `replay` (остальные — в журнале плеера).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryDigest {
    /// `periodic` или `final`.
    pub kind: String,
    /// Сколько кадров показано в окне.
    pub presented_frames: u64,
    /// Позиция воспроизведения, мс.
    pub position_ms: u64,
    /// Состояние плеера (`playing`, `paused`, `buffering` …).
    pub state: String,
}

impl SummaryDigest {
    /// Разбор строки журнала; `None`, если это не сводка или в ней нет нужного поля.
    pub fn parse(line: &str) -> Option<Self> {
        if !line.contains("Browser playback summary") {
            return None;
        }
        Some(Self {
            kind: word_field(line, "kind")?,
            presented_frames: number_field(line, "presented_frames")?,
            position_ms: number_field(line, "position_ms")?,
            state: word_field(line, "state")?,
        })
    }
}

impl std::fmt::Display for SummaryDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: presented_frames={} position_ms={} state={}",
            self.kind, self.presented_frames, self.position_ms, self.state
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_reads_kind_frames_position_and_state() {
        let line = "2026-11-03T10:00:00Z  INFO fastiplayer: Browser playback summary kind=periodic presented_frames=250 taken_frames=251 audio_written_ms=10000 audio_played_ms=9990 audio_output=pipewire position_regressions=0 loads=1 buffering_entries=0 position_ms=9980 state=playing";
        let digest = SummaryDigest::parse(line).expect("a summary line");
        assert_eq!(
            digest,
            SummaryDigest {
                kind: "periodic".into(),
                presented_frames: 250,
                position_ms: 9980,
                state: "playing".into()
            }
        );
        assert_eq!(
            digest.to_string(),
            "periodic: presented_frames=250 position_ms=9980 state=playing"
        );
        assert_eq!(SummaryDigest::parse("INFO something else"), None);
    }
}
