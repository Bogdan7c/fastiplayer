use super::*;

/// Bounded limits одного compressed-video recovery scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VideoBacklogRecoveryScanLimits {
    /// Максимум packets, временно удерживаемых до proven keyframe.
    pub(crate) max_staged_packets: usize,

    /// Максимум retained compressed payload до safe rollback-а.
    pub(crate) max_staged_bytes: usize,

    /// Размер обычной очереди, ниже которого разрешается новый scan после backoff.
    pub(crate) rearm_pending_packets: usize,
}

impl VideoBacklogRecoveryScanLimits {
    /// Небольшой bounded budget для focused state-machine tests.
    #[cfg(test)]
    pub(crate) const fn for_tests() -> Self {
        Self {
            max_staged_packets: 8,
            max_staged_bytes: 1024 * 1024,
            rearm_pending_packets: 2,
        }
    }

    /// Защищает state machine от нулевого/неубывающего runtime-конфига.
    #[must_use]
    fn sanitized(self) -> Self {
        let max_staged_packets = self.max_staged_packets.max(1);
        Self {
            max_staged_packets,
            max_staged_bytes: self.max_staged_bytes.max(1),
            rearm_pending_packets: self
                .rearm_pending_packets
                .min(max_staged_packets.saturating_sub(1)),
        }
    }
}

/// Allocation boundary, который остановил текущий recovery scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoBacklogRecoveryScanLimit {
    /// Достигнут максимум удерживаемых video packets.
    PacketCount,

    /// Достигнут максимум retained compressed payload bytes.
    ByteCount,
}

/// Результат запуска двухфазного поиска video recovery point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoBacklogRecoveryScanStart {
    /// Новый scan запущен; уже накопленная очередь сохранена без изменений.
    Started,

    /// Scan уже был активен, поэтому повторный intent не изменил состояние.
    AlreadyScanning,

    /// Предыдущий bounded scan откатился и ждёт, пока decoder разгрузит очередь.
    BackoffUntilBacklogDrains,

    /// Recovery неприменим без выбранного video track и не был запущен.
    NoSelectedVideo,

    /// Для выбранного track отсутствует resolved codec requirement.
    NoActiveVideoRequirement,

    /// Codec не гарантирует безопасный no-flush cut по общему `PacketKeyframe` contract-у.
    CodecWithoutNoFlushRecoveryProof {
        /// Codec, для которого player-core сознательно оставил обычный FIFO.
        codec: codec_core::VideoCodec,
    },

    /// Контейнер ещё не доказал ни одного keyframe для выбранного video track.
    NoProvenKeyframeObserved,

    /// Decoder уже ожидает bootstrap keyframe после другого discontinuity.
    DecoderAwaitingKeyframe,
}

/// Результат pipeline-owned маршрутизации очередного video packet во время recovery scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoBacklogRecoveryRouteOutcome {
    /// Recovery scan не активен, поэтому packet добавлен в обычный FIFO.
    QueuedNormally,

    /// Packet удержан в bounded staging и может быть восстановлен без разрыва reference chain.
    StagedWhileScanning,

    /// Staging достиг лимита: весь просмотренный continuation возвращён после старого backlog.
    ScanLimitReached {
        /// Какая allocation-граница потребовала rollback.
        limit: VideoBacklogRecoveryScanLimit,

        /// Число packets, восстановленных из staging в pending FIFO.
        restored_staged_packets: usize,

        /// Compressed payload, возвращённый из staging в pending FIFO.
        restored_staged_bytes: usize,

        /// Полный размер pending FIFO после безопасного rollback-а.
        pending_packets_after_restore: usize,
    },

    /// Proven keyframe заменил старый backlog и уже добавлен первым packet нового segment.
    SwitchedAtKeyframe {
        /// Число старых pending packets, удалённых перед enqueue keyframe-а.
        discarded_pending_packets: usize,

        /// Число staged continuation packets, отброшенных при подтверждённом switch-е.
        discarded_staged_packets: usize,

        /// PTS доказанного recovery keyframe для diagnostics и focused tests.
        recovery_keyframe_pts: Duration,
    },
}

/// Accounting штатного EOF, который может вернуть незавершённый scan в FIFO.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VideoBacklogRecoveryEofReport {
    /// Число staged packets, восстановленных перед decoder EOF-drain.
    pub(crate) restored_staged_packets: usize,
}

/// Accounting безопасного downshift из accelerated recovery path-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VideoBacklogRecoveryRateChangeReport {
    /// Число staged packets, возвращённых в FIFO после перехода к `<=1x`.
    pub(crate) restored_staged_packets: usize,
}

/// Внутреннее состояние bounded поиска proven keyframe.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum VideoBacklogRecoveryState {
    /// Обычная маршрутизация video packets без recovery-фильтра.
    #[default]
    Inactive,

    /// Existing backlog сохранён, а новые packets временно удерживаются до keyframe.
    ScanningForKeyframe {
        /// Seek generation, внутри которой начат recovery scan.
        generation: u64,

        /// Выбранный video track, чей keyframe может завершить scan.
        selected_track_id: TrackId,

        /// Bounded staging и rearm thresholds текущего scan-а.
        limits: VideoBacklogRecoveryScanLimits,
    },

    /// Scan безопасно откатился и не перезапускается, пока очередь не разгрузится.
    BackoffUntilBacklogDrains {
        /// Pending threshold, после которого новый scan снова допустим.
        rearm_pending_packets: usize,
    },
}

impl PlaybackPipeline {
    /// Добавляет video packet в pending queue и запоминает доказанный keyframe contract.
    pub(crate) fn enqueue_pending_video_packet(&mut self, packet: PendingVideoPacket) {
        if self.video_packet_belongs_to_selected_track(packet.track_id)
            && packet.keyframe == media_core::PacketKeyframe::Keyframe
        {
            self.video_proven_keyframe_observed_for_track = Some(packet.track_id);
        }
        self.pending_video_packets.push_back(packet);
    }

    /// Забирает первый pending video packet для drop или отправки в decoder.
    pub(crate) fn pop_pending_video_packet_front(&mut self) -> Option<PendingVideoPacket> {
        let packet = self.pending_video_packets.pop_front();
        self.rearm_video_backlog_recovery_after_drain();
        packet
    }

    /// Возвращает первый pending video packet без снятия его с очереди.
    #[must_use]
    pub(crate) fn front_pending_video_packet(&self) -> Option<&PendingVideoPacket> {
        self.pending_video_packets.front()
    }

    /// Проверяет, пуста ли очередь pending video packets.
    #[must_use]
    pub(crate) fn pending_video_packet_is_empty(&self) -> bool {
        self.pending_video_packets.is_empty()
    }

    /// Возвращает число packets в decoder-facing pending FIFO без staged continuation.
    #[must_use]
    pub(crate) fn pending_video_packet_len(&self) -> usize {
        self.pending_video_packets.len()
    }

    /// Возвращает bounded staging size для diagnostics/focused tests.
    #[must_use]
    pub(crate) fn video_backlog_recovery_staged_packet_len(&self) -> usize {
        self.video_backlog_recovery_staged_packets.len()
    }

    /// Возвращает retained compressed payload staging-а без обхода packets.
    #[must_use]
    pub(crate) const fn video_backlog_recovery_staged_bytes(&self) -> usize {
        self.video_backlog_recovery_staged_bytes
    }

    /// Очищает pending queue при настоящем discontinuity и отменяет незавершённый scan.
    pub(crate) fn clear_pending_video_packets(&mut self) {
        self.pending_video_packets.clear();
        self.cancel_video_backlog_recovery_scan_for_discontinuity();
    }

    /// Запускает bounded поиск proven keyframe, не изменяя накопленный backlog.
    pub(crate) fn begin_video_backlog_recovery_scan(
        &mut self,
        limits: VideoBacklogRecoveryScanLimits,
    ) -> VideoBacklogRecoveryScanStart {
        match self.video_backlog_recovery_state {
            VideoBacklogRecoveryState::ScanningForKeyframe { .. } => {
                return VideoBacklogRecoveryScanStart::AlreadyScanning;
            }
            VideoBacklogRecoveryState::BackoffUntilBacklogDrains { .. } => {
                return VideoBacklogRecoveryScanStart::BackoffUntilBacklogDrains;
            }
            VideoBacklogRecoveryState::Inactive => {}
        }

        let Some(selected_track_id) = self.video_track_id else {
            return VideoBacklogRecoveryScanStart::NoSelectedVideo;
        };
        let Some(active_requirement) = self.active_video_requirement.as_ref() else {
            return VideoBacklogRecoveryScanStart::NoActiveVideoRequirement;
        };
        if !matches!(
            active_requirement.codec,
            codec_core::VideoCodec::Av1 | codec_core::VideoCodec::Vp9
        ) {
            return VideoBacklogRecoveryScanStart::CodecWithoutNoFlushRecoveryProof {
                codec: active_requirement.codec,
            };
        }
        if self.video_proven_keyframe_observed_for_track != self.video_track_id {
            return VideoBacklogRecoveryScanStart::NoProvenKeyframeObserved;
        }
        if self.video_decoder_needs_keyframe {
            return VideoBacklogRecoveryScanStart::DecoderAwaitingKeyframe;
        }

        debug_assert!(self.video_backlog_recovery_staged_packets.is_empty());
        debug_assert_eq!(self.video_backlog_recovery_staged_bytes, 0);
        self.video_backlog_recovery_state = VideoBacklogRecoveryState::ScanningForKeyframe {
            generation: self.seek_generation,
            selected_track_id,
            limits: limits.sanitized(),
        };
        VideoBacklogRecoveryScanStart::Started
    }

    /// Разрешает demux продолжить scan через заполненный pending-video лимит.
    #[must_use]
    pub(crate) fn video_backlog_recovery_scan_allows_demux(&self) -> bool {
        matches!(
            self.video_backlog_recovery_state,
            VideoBacklogRecoveryState::ScanningForKeyframe {
                generation,
                selected_track_id,
                ..
            } if generation == self.seek_generation
                && Some(selected_track_id) == self.video_track_id
                && !self.video_decoder_needs_keyframe
        )
    }

    /// На EOF возвращает staged continuation после старого backlog в исходном порядке.
    #[must_use]
    pub(crate) fn finish_video_backlog_recovery_scan_at_eof(
        &mut self,
    ) -> VideoBacklogRecoveryEofReport {
        let restored_staged_packets = self.video_backlog_recovery_staged_packets.len();
        self.pending_video_packets
            .append(&mut self.video_backlog_recovery_staged_packets);
        self.video_backlog_recovery_staged_bytes = 0;
        self.video_backlog_recovery_state = VideoBacklogRecoveryState::Inactive;
        VideoBacklogRecoveryEofReport {
            restored_staged_packets,
        }
    }

    /// Восстанавливает staged continuation после успешного downshift к `<=1x`.
    ///
    /// Метод вызывается только после атомарного tempo/clock commit-а. Ускоренный
    /// rate-to-rate переход сохраняет scan; normal/slow playback не наследует skip.
    #[must_use]
    pub(crate) fn reconcile_video_backlog_recovery_after_rate_change(
        &mut self,
        playback_rate: PlaybackRate,
    ) -> VideoBacklogRecoveryRateChangeReport {
        if playback_rate.is_faster_than_normal() {
            return VideoBacklogRecoveryRateChangeReport::default();
        }

        let VideoBacklogRecoveryState::ScanningForKeyframe { limits, .. } =
            self.video_backlog_recovery_state
        else {
            return VideoBacklogRecoveryRateChangeReport::default();
        };
        let restored_staged_packets = self.video_backlog_recovery_staged_packets.len();
        self.pending_video_packets
            .append(&mut self.video_backlog_recovery_staged_packets);
        self.video_backlog_recovery_staged_bytes = 0;
        self.video_backlog_recovery_state = VideoBacklogRecoveryState::BackoffUntilBacklogDrains {
            rearm_pending_packets: limits.rearm_pending_packets,
        };
        self.rearm_video_backlog_recovery_after_drain();
        VideoBacklogRecoveryRateChangeReport {
            restored_staged_packets,
        }
    }

    /// Сбрасывает proof и scan при смене selected track identity.
    pub(crate) fn reset_video_backlog_recovery_for_selected_track_change(&mut self) {
        self.video_proven_keyframe_observed_for_track = None;
        self.cancel_video_backlog_recovery_scan_for_discontinuity();
    }

    /// Отменяет scan при seek/media/track-list discontinuity; keyframe proof остаётся track-scoped.
    pub(crate) fn cancel_video_backlog_recovery_scan_for_discontinuity(&mut self) {
        self.video_backlog_recovery_staged_packets.clear();
        self.video_backlog_recovery_staged_bytes = 0;
        self.video_backlog_recovery_state = VideoBacklogRecoveryState::Inactive;
    }

    /// Отменяет scan при decoder replacement, после которого session выполняет reseek.
    pub(crate) fn cancel_video_backlog_recovery_scan_for_decoder_replacement(&mut self) {
        self.cancel_video_backlog_recovery_scan_for_discontinuity();
    }

    /// Маршрутизирует packet через bounded staging или proven-keyframe switch.
    pub(crate) fn route_pending_video_packet_for_backlog_recovery(
        &mut self,
        packet: PendingVideoPacket,
    ) -> VideoBacklogRecoveryRouteOutcome {
        let VideoBacklogRecoveryState::ScanningForKeyframe {
            generation,
            selected_track_id,
            limits,
        } = self.video_backlog_recovery_state
        else {
            self.enqueue_pending_video_packet(packet);
            return VideoBacklogRecoveryRouteOutcome::QueuedNormally;
        };

        let packet_is_recovery_keyframe = packet.generation == generation
            && packet.track_id == selected_track_id
            && packet.keyframe == media_core::PacketKeyframe::Keyframe;
        if packet_is_recovery_keyframe {
            let discarded_pending_packets = self.pending_video_packets.len();
            let discarded_staged_packets = self.video_backlog_recovery_staged_packets.len();
            let recovery_keyframe_pts = packet.pts;
            self.pending_video_packets.clear();
            self.video_backlog_recovery_staged_packets.clear();
            self.video_backlog_recovery_staged_bytes = 0;
            self.enqueue_pending_video_packet(packet);
            self.video_backlog_recovery_state = VideoBacklogRecoveryState::Inactive;
            return VideoBacklogRecoveryRouteOutcome::SwitchedAtKeyframe {
                discarded_pending_packets,
                discarded_staged_packets,
                recovery_keyframe_pts,
            };
        }

        self.video_backlog_recovery_staged_bytes = self
            .video_backlog_recovery_staged_bytes
            .saturating_add(packet.encoded_bytes.len());
        self.video_backlog_recovery_staged_packets.push_back(packet);
        let reached_limit =
            if self.video_backlog_recovery_staged_packets.len() >= limits.max_staged_packets {
                Some(VideoBacklogRecoveryScanLimit::PacketCount)
            } else if self.video_backlog_recovery_staged_bytes >= limits.max_staged_bytes {
                Some(VideoBacklogRecoveryScanLimit::ByteCount)
            } else {
                None
            };
        let Some(limit) = reached_limit else {
            return VideoBacklogRecoveryRouteOutcome::StagedWhileScanning;
        };

        let restored_staged_packets = self.video_backlog_recovery_staged_packets.len();
        let restored_staged_bytes = self.video_backlog_recovery_staged_bytes;
        self.pending_video_packets
            .append(&mut self.video_backlog_recovery_staged_packets);
        self.video_backlog_recovery_staged_bytes = 0;
        let pending_packets_after_restore = self.pending_video_packets.len();
        self.video_backlog_recovery_state = VideoBacklogRecoveryState::BackoffUntilBacklogDrains {
            rearm_pending_packets: limits.rearm_pending_packets,
        };
        VideoBacklogRecoveryRouteOutcome::ScanLimitReached {
            limit,
            restored_staged_packets,
            restored_staged_bytes,
            pending_packets_after_restore,
        }
    }

    /// Снимает scan backoff только после заметной разгрузки decoder-facing FIFO.
    fn rearm_video_backlog_recovery_after_drain(&mut self) {
        let VideoBacklogRecoveryState::BackoffUntilBacklogDrains {
            rearm_pending_packets,
        } = self.video_backlog_recovery_state
        else {
            return;
        };
        if self.pending_video_packets.len() <= rearm_pending_packets {
            self.video_backlog_recovery_state = VideoBacklogRecoveryState::Inactive;
        }
    }
}
