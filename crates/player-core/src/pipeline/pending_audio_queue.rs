//! Очередь сжатых audio packets, ждущих декодирования, вместе с учётом её объёма.
//!
//! Пока стартовый preroll ждёт первый видеокадр, звук, прочитанный сверх стартового
//! потолка output-а, остаётся здесь в сжатом виде (decode throttle его не трогает).
//! Session ограничивает этот запас по объёму и по media-длительности, поэтому очередь
//! сама ведёт счётчик байт: пересчёт всей очереди на каждом demux-чтении был бы
//! квадратичным. Все изменения очереди идут только через методы этого типа, поэтому
//! счётчик не может разойтись с содержимым.

use std::collections::VecDeque;
use std::time::Duration;

use super::PendingAudioPacket;

/// FIFO сжатых audio packets с текущим объёмом payload-а.
#[derive(Default)]
pub(crate) struct PendingAudioPacketQueue {
    /// Packets в порядке demux-чтения.
    packets: VecDeque<PendingAudioPacket>,

    /// Сумма длин encoded payload всех packets очереди, байт.
    encoded_bytes: usize,
}

/// Сколько сжатого звука сейчас ждёт декодирования.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PendingAudioBacklog {
    /// Число packets в очереди.
    pub(crate) packets: usize,

    /// Суммарный encoded payload, байт.
    pub(crate) encoded_bytes: usize,

    /// Media-расстояние между началом первого и началом последнего packet-а.
    ///
    /// Длительность последнего packet-а не входит (надёжно она известна только после
    /// декода), поэтому оценка занижена максимум на один packet. Packets не по порядку
    /// (битые PTS) дают ноль, а не переполнение — для такого случая есть предел по байтам.
    pub(crate) media_span: Duration,
}

impl PendingAudioPacketQueue {
    /// Добавляет packet в конец очереди.
    pub(crate) fn push_back(&mut self, packet: PendingAudioPacket) {
        self.encoded_bytes = self
            .encoded_bytes
            .saturating_add(packet.encoded_bytes().len());
        self.packets.push_back(packet);
    }

    /// Забирает первый packet очереди.
    pub(crate) fn pop_front(&mut self) -> Option<PendingAudioPacket> {
        let packet = self.packets.pop_front()?;
        self.encoded_bytes = self
            .encoded_bytes
            .saturating_sub(packet.encoded_bytes().len());
        Some(packet)
    }

    /// Возвращает packet в начало очереди (decode throttle не успел его обработать).
    pub(crate) fn push_front(&mut self, packet: PendingAudioPacket) {
        self.encoded_bytes = self
            .encoded_bytes
            .saturating_add(packet.encoded_bytes().len());
        self.packets.push_front(packet);
    }

    /// Первый packet очереди без извлечения.
    #[must_use]
    pub(crate) fn front(&self) -> Option<&PendingAudioPacket> {
        self.packets.front()
    }

    /// Пуста ли очередь.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    /// Число packets в очереди.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.packets.len()
    }

    /// Убирает из начала очереди packets, которые целиком лежат до `target`.
    ///
    /// Конец packet-а надёжно известен только после декода, поэтому «целиком до цели»
    /// доказывает следующий packet: если он начинается не позже цели, текущий кончился
    /// до неё. Packet, внутри которого лежит цель, остаётся (его PCM обрежется по базе
    /// часов). Packets не по порядку (битые PTS) останавливают удаление: доказательства нет.
    /// Возвращает число удалённых packets.
    pub(crate) fn discard_front_packets_ending_before(&mut self, target: Duration) -> usize {
        let mut discarded_packets = 0;
        while let Some(next_packet) = self.packets.get(1) {
            let front_start = self.packets.front().map(PendingAudioPacket::pts);
            let next_start = next_packet.pts();
            let front_ends_before_target =
                next_start <= target && front_start.is_some_and(|start| start <= next_start);
            if !front_ends_before_target {
                break;
            }
            let _ = self.pop_front();
            discarded_packets += 1;
        }
        discarded_packets
    }

    /// Очищает очередь вместе со счётчиком объёма.
    pub(crate) fn clear(&mut self) {
        self.packets.clear();
        self.encoded_bytes = 0;
    }

    /// Текущий объём ожидающего сжатого звука.
    #[must_use]
    pub(crate) fn backlog(&self) -> PendingAudioBacklog {
        let media_span = match (self.packets.front(), self.packets.back()) {
            (Some(first), Some(last)) => last.pts().saturating_sub(first.pts()),
            _ => Duration::ZERO,
        };
        PendingAudioBacklog {
            packets: self.packets.len(),
            encoded_bytes: self.encoded_bytes,
            media_span,
        }
    }
}
