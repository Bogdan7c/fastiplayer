//! Минимальный TS-мультиплексор для одного аудио-PES: PAT + PMT + PES с PTS.

use super::PackedAudioError;

/// Размер транспортного пакета MPEG-TS.
const TS_PACKET_BYTES: usize = 188;
/// Полезная нагрузка пакета без заголовка и adaptation field.
const TS_PAYLOAD_BYTES: usize = 184;
/// PID таблицы PMT. Значение произвольное: демуксер берёт его из PAT.
const PMT_PID: u16 = 0x1000;
/// PID аудио-потока.
const AUDIO_PID: u16 = 0x0100;
/// Номер единственной программы.
const PROGRAM_NUMBER: u16 = 1;
/// `stream_id` аудио-PES (ISO 13818-1: 110x xxxx).
const AUDIO_STREAM_ID: u8 = 0xc0;
/// `stream_type` AAC в ADTS.
const STREAM_TYPE_AAC_ADTS: u8 = 0x0f;
/// `stream_type` MPEG-1 audio.
const STREAM_TYPE_MPEG1_AUDIO: u8 = 0x03;
/// `stream_type` MPEG-2 audio (низкие частоты дискретизации).
const STREAM_TYPE_MPEG2_AUDIO: u8 = 0x04;
/// Длина PES-заголовка после поля `PES_packet_length` с одним PTS: флаги(2) + длина(1) + PTS(5).
const PES_HEADER_AFTER_LENGTH_BYTES: usize = 8;

/// Кодек packed-сегмента, доказанный sync-словом первого кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PackedAudioCodec {
    AacAdts,
    /// MPEG-1 Layer I/II/III.
    Mpeg1Audio,
    /// MPEG-2/2.5 Layer I/II/III.
    Mpeg2Audio,
}

impl PackedAudioCodec {
    /// Определяет кодек по заголовку первого кадра (уже без ID3).
    pub(super) fn detect(elementary: &[u8]) -> Result<Self, PackedAudioError> {
        match elementary {
            [0x0b, 0x77, ..] => Err(PackedAudioError::UnsupportedCodec),
            [0xff, second, ..] if second & 0xf0 == 0xf0 && (second >> 1) & 0x03 == 0 => {
                Ok(Self::AacAdts)
            }
            // Слой 00 зарезервирован у MPEG audio, версия 01 тоже: эти комбинации - не кадр.
            [0xff, second, ..]
                if second & 0xe0 == 0xe0
                    && (second >> 1) & 0x03 != 0
                    && (second >> 3) & 0x03 != 1 =>
            {
                if (second >> 3) & 0x03 == 0x03 {
                    Ok(Self::Mpeg1Audio)
                } else {
                    Ok(Self::Mpeg2Audio)
                }
            }
            _ => Err(PackedAudioError::MissingAudioSync),
        }
    }

    const fn stream_type(self) -> u8 {
        match self {
            Self::AacAdts => STREAM_TYPE_AAC_ADTS,
            Self::Mpeg1Audio => STREAM_TYPE_MPEG1_AUDIO,
            Self::Mpeg2Audio => STREAM_TYPE_MPEG2_AUDIO,
        }
    }
}

/// Строит audio-only TS: PAT, PMT и один PES со всем аудио сегмента и PTS первого кадра.
///
/// PTS остальных кадров демуксер накапливает сам по длительности кадра. Если PES не помещается
/// в 16-битное поле длины, пишется `PES_packet_length = 0` (разрешено для потоковых PES; парсер
/// проекта читает такой PES до следующего start-индикатора или конца ресурса).
pub(super) fn wrap_in_transport_stream(
    elementary: &[u8],
    codec: PackedAudioCodec,
    first_pts_ticks: u64,
) -> Vec<u8> {
    let packet_count = 2 + (elementary.len() + 14).div_ceil(TS_PAYLOAD_BYTES);
    let mut writer = PacketWriter::with_capacity(packet_count * TS_PACKET_BYTES);
    let mut pat = PidState::new(0);
    let mut pmt = PidState::new(PMT_PID);
    let mut audio = PidState::new(AUDIO_PID);

    writer.push_section(&mut pat, &program_association_table());
    writer.push_section(&mut pmt, &program_map_table(codec));
    writer.push_pes(&mut audio, &audio_pes(elementary, first_pts_ticks));
    writer.bytes
}

/// PAT с одной программой, указывающей на [`PMT_PID`].
fn program_association_table() -> Vec<u8> {
    let mut section = vec![0x00, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00];
    section.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes());
    section.push(0xe0 | ((PMT_PID >> 8) as u8 & 0x1f));
    section.push(PMT_PID as u8);
    finalize_section(&mut section);
    section
}

/// PMT с единственным аудио-потоком; PCR_PID указывает на него же (PCR не передаётся).
fn program_map_table(codec: PackedAudioCodec) -> Vec<u8> {
    let mut section = vec![0x02, 0xb0, 0x00];
    section.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes());
    section.extend_from_slice(&[0xc1, 0x00, 0x00]);
    section.push(0xe0 | ((AUDIO_PID >> 8) as u8 & 0x1f));
    section.push(AUDIO_PID as u8);
    // program_info_length = 0
    section.extend_from_slice(&[0xf0, 0x00]);
    section.push(codec.stream_type());
    section.push(0xe0 | ((AUDIO_PID >> 8) as u8 & 0x1f));
    section.push(AUDIO_PID as u8);
    // ES_info_length = 0
    section.extend_from_slice(&[0xf0, 0x00]);
    finalize_section(&mut section);
    section
}

/// Дописывает `section_length` и CRC32 к PSI-секции.
fn finalize_section(section: &mut Vec<u8>) {
    // section_length считается после самого поля и включает 4 байта CRC.
    let section_length = section.len() - 3 + 4;
    section[1] = 0xb0 | ((section_length >> 8) as u8 & 0x0f);
    section[2] = section_length as u8;
    section.extend_from_slice(&mpeg_crc32(section).to_be_bytes());
}

/// Собирает PES с PTS и всем elementary stream сегмента.
fn audio_pes(elementary: &[u8], pts_ticks: u64) -> Vec<u8> {
    let mut pes = Vec::with_capacity(6 + PES_HEADER_AFTER_LENGTH_BYTES + elementary.len());
    pes.extend_from_slice(&[0x00, 0x00, 0x01, AUDIO_STREAM_ID]);
    let packet_length = PES_HEADER_AFTER_LENGTH_BYTES + elementary.len();
    let declared_length = u16::try_from(packet_length).unwrap_or(0);
    pes.extend_from_slice(&declared_length.to_be_bytes());
    // marker `10`, без scrambling/priority; PTS_DTS_flags = `10` (только PTS); header_data_length = 5.
    pes.extend_from_slice(&[0x80, 0x80, 0x05]);
    pes.extend_from_slice(&encode_pts(pts_ticks));
    pes.extend_from_slice(elementary);
    pes
}

/// 33-битный PTS в 5-байтовой записи PES с marker-битами (prefix `0010`).
fn encode_pts(ticks: u64) -> [u8; 5] {
    let ticks = ticks & ((1_u64 << 33) - 1);
    [
        0x20 | (((ticks >> 30) as u8 & 0x07) << 1) | 1,
        (ticks >> 22) as u8,
        (((ticks >> 15) as u8 & 0x7f) << 1) | 1,
        (ticks >> 7) as u8,
        ((ticks as u8 & 0x7f) << 1) | 1,
    ]
}

/// Счётчик непрерывности одного PID.
struct PidState {
    pid: u16,
    continuity_counter: u8,
}

impl PidState {
    const fn new(pid: u16) -> Self {
        Self {
            pid,
            continuity_counter: 0,
        }
    }
}

struct PacketWriter {
    bytes: Vec<u8>,
}

impl PacketWriter {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    /// PSI-секция идёт в одном пакете с `pointer_field = 0`.
    fn push_section(&mut self, pid: &mut PidState, section: &[u8]) {
        let mut payload = Vec::with_capacity(section.len() + 1);
        payload.push(0);
        payload.extend_from_slice(section);
        self.push_payload(pid, true, &payload);
    }

    /// PES режется на 184-байтовые payload-ы; первый несёт payload_unit_start.
    fn push_pes(&mut self, pid: &mut PidState, pes: &[u8]) {
        for (index, chunk) in pes.chunks(TS_PAYLOAD_BYTES).enumerate() {
            self.push_payload(pid, index == 0, chunk);
        }
    }

    /// Пишет один пакет; короткий хвост добивается stuffing-ом в adaptation field.
    fn push_payload(&mut self, pid: &mut PidState, payload_unit_start: bool, payload: &[u8]) {
        debug_assert!(payload.len() <= TS_PAYLOAD_BYTES);
        let mut packet = [0xff_u8; TS_PACKET_BYTES];
        packet[0] = 0x47;
        packet[1] = (u8::from(payload_unit_start) << 6) | ((pid.pid >> 8) as u8 & 0x1f);
        packet[2] = pid.pid as u8;
        let counter = pid.continuity_counter;
        pid.continuity_counter = (counter + 1) & 0x0f;
        if payload.len() < TS_PAYLOAD_BYTES {
            let adaptation_length = TS_PAYLOAD_BYTES - 1 - payload.len();
            packet[3] = 0x30 | counter;
            packet[4] = adaptation_length as u8;
            if adaptation_length > 0 {
                // Флаги adaptation field пустые, остальное - 0xff stuffing.
                packet[5] = 0;
            }
            let payload_offset = 5 + adaptation_length;
            packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        } else {
            packet[3] = 0x10 | counter;
            packet[4..].copy_from_slice(payload);
        }
        self.bytes.extend_from_slice(&packet);
    }
}

/// CRC32 MPEG-2 (полином 0x04C11DB7, начальное значение из единиц).
fn mpeg_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}
