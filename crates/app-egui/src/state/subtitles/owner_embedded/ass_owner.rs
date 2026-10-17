//! Владелец и встроенный ASS: приём заголовков дорожек и выбор пути разбора пакета.
//!
//! Логика состояния (заголовки, сценарии, `ReadOrder`) живёт в [`super::ass_tracks`]; здесь
//! только связка с `SubtitleOwner`: счётчики, лог и сброс идентичности источника.
//!
//! Отказ любого шага не останавливает воспроизведение: дорожка без сценария продолжает
//! показываться запасным путём «только текст» (счётчик + `warn!` один раз, без содержимого).

use player_core::{SubtitleFeedPacket, SubtitleTrackHeader};
use subtitle_formats::{AssParseError, EmbeddedCueFormat};

use super::ass_tracks::{AssPacketLimits, AssScriptAvailability, HeaderStash};
use crate::state::subtitles::SubtitleOwner;
use crate::state::subtitles::embedded_events::{PacketEventOutcome, event_from_packet};
use crate::state::subtitles::track_list::TrackLoadState;

impl SubtitleOwner {
    /// Запоминает заголовки дорожек из `MediaStarted` текущего media.
    ///
    /// Если заголовок дорожки заменяет прежний (штатно не бывает: `MediaStarted` один на
    /// media), её кэш реплик начинается заново под новым `CueSourceId`: контракт `CueKey`
    /// «тот же ключ = то же содержимое» не допускает старых ключей при новом сценарии.
    pub(super) fn stash_ass_track_headers(&mut self, headers: &[SubtitleTrackHeader]) {
        let Some(media) = self.media.as_mut() else {
            return;
        };
        let mut restarted_any_track = false;
        for header in headers {
            let stash = media
                .embedded_ass
                .stash_header(header.track_id, header.codec_private.clone());
            if stash != HeaderStash::ReplacedPrevious {
                continue;
            }
            let Some(track_id) = media.tracks.find_embedded(header.track_id) else {
                continue;
            };
            if let Some(TrackLoadState::Ready(track)) =
                media.tracks.get_mut(track_id).map(|entry| &mut entry.load)
            {
                track.restart_with_source(self.source_ids.allocate());
                restarted_any_track = true;
            }
        }
        if restarted_any_track {
            self.presenter.forget_shown_overlay();
        }
    }

    /// Разбирает пакет встроенной дорожки в исход: ASS-путь для дорожки со сценарием,
    /// иначе прежний путь «только текст».
    pub(super) fn packet_event_outcome(
        &mut self,
        packet: &SubtitleFeedPacket,
        cue_format: EmbeddedCueFormat,
    ) -> PacketEventOutcome {
        let limits = self.settings.embedded_limits;
        if cue_format == EmbeddedCueFormat::AssEventLine
            && let Some(media) = self.media.as_mut()
        {
            let availability = media
                .embedded_ass
                .ensure_script(packet.track_id, &limits.ass_parse);
            if let AssScriptAvailability::JustRejected(error) = availability {
                self.note_ass_header_rejected(error);
            } else if availability == AssScriptAvailability::Ready
                && let Some(outcome) = self.ass_event_outcome(packet)
            {
                return outcome;
            }
        }
        event_from_packet(
            packet,
            cue_format,
            &limits.cue_parse,
            limits.cue_max_open_duration,
        )
    }

    /// ASS-исход пакета по разобранному сценарию его дорожки.
    fn ass_event_outcome(&mut self, packet: &SubtitleFeedPacket) -> Option<PacketEventOutcome> {
        let limits = self.settings.embedded_limits;
        let media = self.media.as_mut()?;
        media.embedded_ass.event_from_packet(
            packet,
            AssPacketLimits {
                cue_parse: limits.cue_parse,
                ass_parse: limits.ass_parse,
                max_remembered_read_orders: limits.max_cues_per_track,
            },
        )
    }

    /// Считает отказанный заголовок и пишет его в лог без содержимого.
    fn note_ass_header_rejected(&mut self, error: AssParseError) {
        let counters = &mut self.embedded_feed.counters;
        counters.ass_headers_rejected += 1;
        tracing::warn!(
            %error,
            "An ASS header was rejected; the track is shown as plain text instead"
        );
    }
}
