//! Тестовая фикстура: минимальный MPEG-TS с H.264 + AAC, собранный в памяти.
//!
//! Копия `app-egui::media_open::local::tests::mpeg_ts_h264_aac_bytes` (байт-в-байт
//! тот же поток). Нужна, потому что тесты `local_media` переехали в этот crate и
//! больше не видят тестовые модули `app-egui`. Источник без внешних файлов:
//! PAT/PMT + по одному PES-пакету видео и аудио.

/// Собирает TS-поток: PAT → PMT (H.264 на PID 0x101, AAC на PID 0x102) → два PES.
pub(super) fn mpeg_ts_h264_aac_bytes() -> Vec<u8> {
    let pat = psi_section(vec![
        0x00, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xe1, 0x00,
    ]);
    let pmt = psi_section(vec![
        0x02, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00, 0xe1, 0x01, 0xf0, 0x00, 0x1b, 0xe1, 0x01,
        0xf0, 0x00, 0x0f, 0xe1, 0x02, 0xf0, 0x00,
    ]);
    let h264 = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00, 0x01,
        0x65, 0x88,
    ];
    let aac = [0xff, 0xf1, 0x50, 0x80, 0x01, 0x3f, 0xfc, 0x11, 0x22];
    let mut fixture = Vec::new();
    fixture.extend(ts_packet(0, true, 0, &with_pointer(pat)));
    fixture.extend(ts_packet(0x100, true, 0, &with_pointer(pmt)));
    fixture.extend(ts_packet(0x101, true, 0, &pes_bytes(90_000, &h264)));
    fixture.extend(ts_packet(0x102, true, 0, &pes_bytes(90_000, &aac)));
    fixture
}

/// Дописывает длину секции и CRC32 (полином MPEG-2 0x04C11DB7).
fn psi_section(mut section: Vec<u8>) -> Vec<u8> {
    let section_length = section.len() - 3 + 4;
    section[1] = 0xb0 | ((section_length >> 8) as u8 & 0x0f);
    section[2] = section_length as u8;
    let mut crc = 0xffff_ffff_u32;
    for byte in &section {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    section.extend_from_slice(&crc.to_be_bytes());
    section
}

/// Добавляет pointer_field = 0 перед PSI-секцией.
fn with_pointer(section: Vec<u8>) -> Vec<u8> {
    let mut payload = vec![0];
    payload.extend(section);
    payload
}

/// Собирает PES-пакет с PTS (stream_id 0xE0) вокруг payload.
fn pes_bytes(pts: u64, payload: &[u8]) -> Vec<u8> {
    let timestamp = [
        0x21 | (((pts >> 30) as u8 & 0x07) << 1),
        (pts >> 22) as u8,
        (((pts >> 15) as u8 & 0x7f) << 1) | 1,
        (pts >> 7) as u8,
        ((pts as u8 & 0x7f) << 1) | 1,
    ];
    let mut pes = vec![0x00, 0x00, 0x01, 0xe0, 0x00, 0x00, 0x80, 0x80, 0x05];
    pes.extend_from_slice(&timestamp);
    pes.extend_from_slice(payload);
    let length = (pes.len() - 6) as u16;
    pes[4..6].copy_from_slice(&length.to_be_bytes());
    pes
}

/// Упаковывает payload в один 188-байтный TS-пакет, добивая adaptation field.
fn ts_packet(pid: u16, payload_start: bool, continuity: u8, payload: &[u8]) -> [u8; 188] {
    let mut packet = [0xff_u8; 188];
    packet[0] = 0x47;
    packet[1] = ((payload_start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
    packet[2] = pid as u8;
    packet[3] = 0x30 | (continuity & 0x0f);
    let adaptation_length = 183 - payload.len();
    packet[4] = adaptation_length as u8;
    if adaptation_length > 0 {
        packet[5] = 0;
    }
    let payload_offset = 5 + adaptation_length;
    packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
    packet
}
