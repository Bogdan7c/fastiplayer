//! Номера описаний дорожек: смена качества или кодека без сброса плеера («эстафета», 03d).
//!
//! Demuxer посреди потока сообщает `TracksChanged`: у выбранной дорожки новое
//! описание (другие SPS/PPS, разрешение, кодек). Раньше player сбрасывал всё в
//! момент разбора и терял кадры и звук, уже разобранные впрок. Теперь смена —
//! отметка в очереди, а не сброс:
//! - каждое новое описание видео или звука получает следующий номер
//!   ([`TrackDescriptionEpoch`]) и ждёт в очереди описаний своего вида;
//! - каждый packet, прочитанный после этого, помечен номером описания, по
//!   которому его надо декодировать;
//! - decoder помнит номер описания, на которое он настроен. Packet с номером
//!   новее — граница: старый decoder доигрывает всё до неё, затем session
//!   перенастраивает его на описание этого packet-а.
//!
//! Модуль владеет только учётом номеров и описаний. Решения «нужна ли эстафета»,
//! выжимание decoder-а и его перенастройка — у session
//! (`session/track_transition.rs`, `session/tick/video_track_handoff.rs`).
//!
//! Инварианты:
//! - номера одного вида растут строго на единицу; номер packet-а никогда не
//!   новее последнего принятого описания;
//! - для каждого номера новее, чем у decoder-а, в очереди лежит ровно одно
//!   описание; очередь ограничена [`MAX_PENDING_TRACK_DESCRIPTIONS`];
//! - сброс media или старый сброс по `TracksChanged` обнуляют учёт целиком:
//!   после него все packet-ы и decoder относятся к одному описанию.

use std::collections::VecDeque;

use media_core::TrackInfo;

use super::{PendingAudioPacket, PendingVideoPacket, PlaybackPipeline};

/// Сколько описаний одного вида может ждать передачи decoder-у одновременно.
///
/// Обычно ждёт одно: следующая смена качества приходит через секунды. Предел
/// защищает от источника, который меняет описание быстрее, чем decoder успевает
/// выжиматься (страница-враг в мосте): сверх предела session выбирает старый сброс.
pub(crate) const MAX_PENDING_TRACK_DESCRIPTIONS: usize = 4;

/// Номер описания дорожки одного вида внутри текущего media.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TrackDescriptionEpoch(u64);

impl TrackDescriptionEpoch {
    /// Описание, с которым media открыт или пересобран старым сбросом.
    pub(crate) const INITIAL: Self = Self(0);

    /// Номер следующего описания; u64 не переполняется за жизнь процесса.
    #[must_use]
    const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Описание, которое уже принято от demuxer-а, но decoder на него ещё не настроен.
#[derive(Debug, Clone)]
struct PendingTrackDescription {
    epoch: TrackDescriptionEpoch,
    track: TrackInfo,
}

/// Учёт описаний одного вида дорожки (видео или звук).
#[derive(Debug, Default)]
struct KindDescriptionLedger {
    /// Номер последнего принятого описания: им помечаются новые packet-ы.
    admitted: TrackDescriptionEpoch,

    /// Номер описания, на которое настроен decoder.
    decoder: TrackDescriptionEpoch,

    /// Описания новее decoder-а по возрастанию номера.
    pending: VecDeque<PendingTrackDescription>,
}

impl KindDescriptionLedger {
    /// Принимает новое описание и возвращает его номер.
    fn admit(&mut self, track: TrackInfo) -> TrackDescriptionAdmission {
        if self.pending.len() >= MAX_PENDING_TRACK_DESCRIPTIONS {
            return TrackDescriptionAdmission::QueueFull;
        }
        self.admitted = self.admitted.next();
        self.pending.push_back(PendingTrackDescription {
            epoch: self.admitted,
            track,
        });
        TrackDescriptionAdmission::Admitted(self.admitted)
    }

    /// Описание для packet-а с номером `packet_epoch`, если оно новее decoder-а.
    fn description_newer_than_decoder(
        &self,
        packet_epoch: TrackDescriptionEpoch,
    ) -> Option<&PendingTrackDescription> {
        if packet_epoch <= self.decoder {
            return None;
        }
        self.pending
            .iter()
            .find(|description| description.epoch == packet_epoch)
    }

    /// Отмечает, что decoder настроен на `epoch`; более старые описания больше не нужны.
    fn mark_decoder_configured_for(&mut self, epoch: TrackDescriptionEpoch) {
        self.decoder = epoch;
        self.pending.retain(|description| description.epoch > epoch);
    }

    /// Забирает самое новое описание и считает decoder настроенным на него.
    fn settle_to_latest(&mut self) -> Option<TrackInfo> {
        let latest = self.pending.pop_back()?;
        self.pending.clear();
        self.decoder = latest.epoch;
        Some(latest.track)
    }

    fn can_admit(&self) -> bool {
        self.pending.len() < MAX_PENDING_TRACK_DESCRIPTIONS
    }
}

/// Результат приёма нового описания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackDescriptionAdmission {
    /// Описание принято; новые packet-ы помечаются этим номером.
    Admitted(TrackDescriptionEpoch),

    /// Слишком много описаний ждут decoder: caller выбирает старый сброс.
    QueueFull,
}

/// Учёт описаний видео и звука текущего media.
#[derive(Debug, Default)]
pub(crate) struct TrackDescriptionLedger {
    video: KindDescriptionLedger,
    audio: KindDescriptionLedger,
}

impl PendingVideoPacket {
    /// Помечает packet номером описания, по которому его надо декодировать.
    #[must_use]
    pub(crate) fn with_track_description(mut self, epoch: TrackDescriptionEpoch) -> Self {
        self.track_description = epoch;
        self
    }
}

impl PendingAudioPacket {
    /// Помечает packet номером описания, по которому его надо декодировать.
    #[must_use]
    pub(crate) fn with_track_description(mut self, epoch: TrackDescriptionEpoch) -> Self {
        self.track_description = epoch;
        self
    }

    /// Номер описания звука, по которому декодируется packet.
    #[must_use]
    pub(crate) const fn track_description(&self) -> TrackDescriptionEpoch {
        self.track_description
    }
}

impl PlaybackPipeline {
    /// Заменяет список дорожек, НЕ трогая decoders, очереди и output.
    ///
    /// Нужен эстафете: UI и snapshot сразу видят новое описание, а decoder
    /// перейдёт на него на границе показа по номерам описаний.
    pub(crate) fn replace_track_list_keeping_decoders(&mut self, tracks: Vec<TrackInfo>) {
        self.tracks = tracks;
    }

    /// Хватит ли места в очереди описаний видео ещё для одной смены.
    #[must_use]
    pub(crate) fn can_admit_video_track_description(&self) -> bool {
        self.track_descriptions.video.can_admit()
    }

    /// Хватит ли места в очереди описаний звука ещё для одной смены.
    #[must_use]
    pub(crate) fn can_admit_audio_track_description(&self) -> bool {
        self.track_descriptions.audio.can_admit()
    }

    /// Принимает новое описание выбранного видео.
    pub(crate) fn admit_video_track_description(
        &mut self,
        track: TrackInfo,
    ) -> TrackDescriptionAdmission {
        self.track_descriptions.video.admit(track)
    }

    /// Принимает новое описание выбранного звука.
    pub(crate) fn admit_audio_track_description(
        &mut self,
        track: TrackInfo,
    ) -> TrackDescriptionAdmission {
        self.track_descriptions.audio.admit(track)
    }

    /// Номер, которым помечается только что прочитанный video packet.
    #[must_use]
    pub(crate) const fn admitted_video_track_description(&self) -> TrackDescriptionEpoch {
        self.track_descriptions.video.admitted
    }

    /// Номер, которым помечается только что прочитанный audio packet.
    #[must_use]
    pub(crate) const fn admitted_audio_track_description(&self) -> TrackDescriptionEpoch {
        self.track_descriptions.audio.admitted
    }

    /// Первый ожидающий video packet, если его можно отправить текущему decoder-у.
    ///
    /// Packet нового описания — шлагбаум: `None`, пока decoder не передан новому
    /// описанию. Для send loop это выглядит как пустая очередь, поэтому packet
    /// нового эпизода физически не может попасть в старый decoder.
    #[must_use]
    pub(crate) fn front_sendable_video_packet(&self) -> Option<&PendingVideoPacket> {
        self.front_pending_video_packet()
            .filter(|packet| packet.track_description <= self.track_descriptions.video.decoder)
    }

    /// Описание, на которое нужно передать video decoder перед первым packet-ом очереди.
    #[must_use]
    pub(crate) fn pending_video_track_handoff(&self) -> Option<&TrackInfo> {
        let packet = self.front_pending_video_packet()?;
        self.track_descriptions
            .video
            .description_newer_than_decoder(packet.track_description)
            .map(|description| &description.track)
    }

    /// Первый packet очереди принадлежит описанию новее decoder-а, но самого
    /// описания в очереди нет. Нарушение инварианта учёта; session лечит его
    /// перенастройкой на последнее описание, а не вечным ожиданием.
    #[must_use]
    pub(crate) fn front_video_packet_has_unknown_track_description(&self) -> bool {
        self.front_pending_video_packet().is_some_and(|packet| {
            packet.track_description > self.track_descriptions.video.decoder
                && self.pending_video_track_handoff().is_none()
        })
    }

    /// Отмечает, что video decoder настроен на описание первого packet-а очереди.
    pub(crate) fn complete_video_track_handoff(&mut self) {
        let Some(epoch) = self
            .front_pending_video_packet()
            .map(|packet| packet.track_description)
        else {
            return;
        };
        self.track_descriptions
            .video
            .mark_decoder_configured_for(epoch);
    }

    /// После seek flush: самое новое видео-описание, на которое надо настроить decoder.
    ///
    /// Flush уже выбросил всё старое, а packet-ы после seek-а относятся к
    /// последнему описанию. Поэтому decoder сразу настраивается на него, без
    /// выжимания. `None` — decoder и так на последнем описании.
    pub(crate) fn settle_video_track_descriptions_after_flush(&mut self) -> Option<TrackInfo> {
        self.track_descriptions.video.settle_to_latest()
    }

    /// Описание звука, на которое нужно перейти перед decode этого packet-а.
    ///
    /// Звук декодируется строго по очереди, поэтому переход — прямо здесь:
    /// возвращённое описание считается применённым.
    pub(crate) fn take_audio_track_description_for_packet(
        &mut self,
        packet_epoch: TrackDescriptionEpoch,
    ) -> Option<TrackInfo> {
        let track = self
            .track_descriptions
            .audio
            .description_newer_than_decoder(packet_epoch)?
            .track
            .clone();
        self.track_descriptions
            .audio
            .mark_decoder_configured_for(packet_epoch);
        Some(track)
    }

    /// Обнуляет учёт: decoders и все будущие packet-ы относятся к одному описанию.
    pub(crate) fn reset_track_descriptions(&mut self) {
        self.track_descriptions = TrackDescriptionLedger::default();
    }
}
