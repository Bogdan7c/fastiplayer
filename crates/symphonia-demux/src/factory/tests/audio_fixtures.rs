//! Компактные hermetic audio-container fixtures для S28C.
//!
//! Builders ниже создают только минимальные заголовки и payload, нужные existing
//! Symphonia 0.6 readers. Они не являются вторым container parser-ом и не входят
//! в production build.

/// Описание одного generated audio-container fixture.
pub(super) struct AudioContainerFixture {
    /// Stable container ID production demux registry.
    pub(super) container_id: &'static str,
    /// Extension используется только для conflicting-hint checks и temp path.
    pub(super) extension: &'static str,
    /// Neutral codec ID, который публикует `track_mapper`.
    pub(super) codec_id: &'static str,
    /// Полные bytes generated fixture-а.
    pub(super) bytes: Vec<u8>,
    /// Exact codec-private bytes, если format reader обязан их опубликовать.
    pub(super) codec_private: Option<Vec<u8>>,
    /// Exact bytes первого encoded packet-а.
    pub(super) first_packet: Vec<u8>,
    /// Duration первого packet-а в track time-base units.
    pub(super) first_packet_duration_units: u64,
    /// Sample rate generated audio track-а.
    pub(super) sample_rate: u32,
    /// Есть ли authoritative container duration до чтения packet-ов.
    pub(super) duration_is_known: bool,
    /// Сохраняется ли authoritative duration без seekable tail scan-а.
    pub(super) streaming_duration_is_known: bool,
}

/// Возвращает все S28C rows без произвольного Cartesian product-а.
pub(super) fn fixtures() -> Vec<AudioContainerFixture> {
    vec![
        generated_ogg_opus(),
        generated_caf_pcm(),
        generated_wav_pcm(),
        generated_aiff_pcm(),
        generated_native_flac(),
        generated_mpeg_audio(MpegAudioLayer::Layer1),
        generated_mpeg_audio(MpegAudioLayer::Layer2),
        generated_mpeg_audio(MpegAudioLayer::Layer3),
    ]
}

/// Один mono PCM payload используется только внутри container-specific framing.
fn pcm_samples_little_endian() -> Vec<u8> {
    (0_i16..32_i16)
        .flat_map(i16::to_le_bytes)
        .collect::<Vec<_>>()
}

/// Генерирует RIFF/WAVE PCM с известным количеством frames.
fn generated_wav_pcm() -> AudioContainerFixture {
    let sample_data = pcm_samples_little_endian();
    let riff_size = 36_u32 + sample_data.len() as u32;
    let mut bytes = Vec::with_capacity(44 + sample_data.len());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_size.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8_000_u32.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(sample_data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&sample_data);
    AudioContainerFixture {
        container_id: "wave",
        extension: "wav",
        codec_id: "A_PCM_S16LE",
        bytes,
        codec_private: None,
        first_packet: sample_data,
        first_packet_duration_units: 32,
        sample_rate: 8_000,
        duration_is_known: true,
        streaming_duration_is_known: true,
    }
}

/// Генерирует classic AIFF PCM; sample rate 8000 представлен exact 80-bit float.
fn generated_aiff_pcm() -> AudioContainerFixture {
    let sample_data_le = pcm_samples_little_endian();
    let sample_data_be = sample_data_le
        .chunks_exact(2)
        .flat_map(|sample| [sample[1], sample[0]])
        .collect::<Vec<_>>();
    let form_size = 4_u32 + 8 + 18 + 8 + 8 + sample_data_be.len() as u32;
    let mut bytes = Vec::with_capacity(form_size as usize + 8);
    bytes.extend_from_slice(b"FORM");
    bytes.extend_from_slice(&form_size.to_be_bytes());
    bytes.extend_from_slice(b"AIFF");
    bytes.extend_from_slice(b"COMM");
    bytes.extend_from_slice(&18_u32.to_be_bytes());
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&32_u32.to_be_bytes());
    bytes.extend_from_slice(&16_u16.to_be_bytes());
    bytes.extend_from_slice(&[0x40, 0x0b, 0xfa, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(b"SSND");
    bytes.extend_from_slice(&(8_u32 + sample_data_be.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&sample_data_be);
    AudioContainerFixture {
        container_id: "aiff",
        extension: "aiff",
        codec_id: "A_PCM_S16BE",
        bytes,
        codec_private: None,
        first_packet: sample_data_be,
        first_packet_duration_units: 32,
        sample_rate: 8_000,
        duration_is_known: true,
        streaming_duration_is_known: true,
    }
}

/// Генерирует CAF Linear PCM с fixed one-frame packets.
fn generated_caf_pcm() -> AudioContainerFixture {
    let sample_data = pcm_samples_little_endian();
    let mut bytes = Vec::with_capacity(68 + sample_data.len());
    bytes.extend_from_slice(b"caff");
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(b"desc");
    bytes.extend_from_slice(&32_i64.to_be_bytes());
    bytes.extend_from_slice(&8_000_f64.to_be_bytes());
    bytes.extend_from_slice(b"lpcm");
    bytes.extend_from_slice(&2_u32.to_be_bytes());
    bytes.extend_from_slice(&2_u32.to_be_bytes());
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes.extend_from_slice(&16_u32.to_be_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(4_i64 + sample_data.len() as i64).to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&sample_data);
    AudioContainerFixture {
        container_id: "caf",
        extension: "caf",
        codec_id: "A_PCM_S16LE",
        bytes,
        codec_private: None,
        first_packet: sample_data,
        first_packet_duration_units: 32,
        sample_rate: 8_000,
        duration_is_known: true,
        streaming_duration_is_known: true,
    }
}

/// Генерирует Ogg Opus с identification, comment и одним 20 ms silence packet-ом.
fn generated_ogg_opus() -> AudioContainerFixture {
    let (bytes, opus_head, audio_packet) = ogg_opus_stream(0x5332_3843);
    AudioContainerFixture {
        container_id: "ogg",
        extension: "ogg",
        codec_id: "A_OPUS",
        bytes,
        codec_private: Some(opus_head),
        first_packet: audio_packet,
        first_packet_duration_units: 960,
        sample_rate: 48_000,
        duration_is_known: true,
        streaming_duration_is_known: false,
    }
}

/// Генерирует два consecutive Ogg physical streams для ResetRequired proof-а.
pub(super) fn chained_ogg_opus() -> Vec<u8> {
    let (mut first_stream, _, _) = ogg_opus_stream(0x5332_3843);
    let (second_stream, _, _) = ogg_opus_stream(0x5332_3844);
    first_stream.extend_from_slice(&second_stream);
    first_stream
}

/// Строит один complete Ogg Opus physical stream с unique serial.
fn ogg_opus_stream(stream_serial: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut opus_head = Vec::with_capacity(19);
    opus_head.extend_from_slice(b"OpusHead");
    opus_head.push(1);
    opus_head.push(1);
    opus_head.extend_from_slice(&0_u16.to_le_bytes());
    opus_head.extend_from_slice(&48_000_u32.to_le_bytes());
    opus_head.extend_from_slice(&0_i16.to_le_bytes());
    opus_head.push(0);
    let mut opus_tags = Vec::with_capacity(16);
    opus_tags.extend_from_slice(b"OpusTags");
    opus_tags.extend_from_slice(&0_u32.to_le_bytes());
    opus_tags.extend_from_slice(&0_u32.to_le_bytes());
    let audio_packet = vec![0xf8, 0xff, 0xfe];
    let mut bytes = ogg_page(0x02, 0, stream_serial, 0, &opus_head);
    bytes.extend_from_slice(&ogg_page(0, 0, stream_serial, 1, &opus_tags));
    bytes.extend_from_slice(&ogg_page(0x04, 960, stream_serial, 2, &audio_packet));
    (bytes, opus_head, audio_packet)
}

/// Строит одну single-packet Ogg page и вычисляет обязательный Ogg CRC.
fn ogg_page(
    header_type: u8,
    granule_position: u64,
    stream_serial: u32,
    sequence: u32,
    packet: &[u8],
) -> Vec<u8> {
    assert!(
        packet.len() < 255,
        "S28C Ogg packet должен иметь один lacing segment"
    );
    let mut page = Vec::with_capacity(28 + packet.len());
    page.extend_from_slice(b"OggS");
    page.push(0);
    page.push(header_type);
    page.extend_from_slice(&granule_position.to_le_bytes());
    page.extend_from_slice(&stream_serial.to_le_bytes());
    page.extend_from_slice(&sequence.to_le_bytes());
    page.extend_from_slice(&0_u32.to_le_bytes());
    page.push(1);
    page.push(packet.len() as u8);
    page.extend_from_slice(packet);
    let checksum = ogg_crc(&page);
    page[22..26].copy_from_slice(&checksum.to_le_bytes());
    page
}

/// Вычисляет Ogg CRC-32 polynomial `0x04c11db7` без внешней test dependency.
fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut checksum = 0_u32;
    for byte in bytes {
        checksum ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            checksum = if checksum & 0x8000_0000 != 0 {
                (checksum << 1) ^ 0x04c1_1db7
            } else {
                checksum << 1
            };
        }
    }
    checksum
}

/// Генерирует native FLAC STREAMINFO и два constant-subframe packets.
fn generated_native_flac() -> AudioContainerFixture {
    let stream_info = vec![
        0x00, 0x10, 0x00, 0x10, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x0c, 0x01, 0xf4, 0x00, 0xf0, 0x00,
        0x00, 0x00, 0x20, 0x3b, 0x5d, 0x3c, 0x7d, 0x20, 0x7e, 0x37, 0xdc, 0xee, 0xed, 0xd3, 0x01,
        0xe3, 0x5e, 0x2e, 0x58,
    ];
    let first_frame = vec![
        0xff, 0xf8, 0x64, 0x08, 0x00, 0x0f, 0xce, 0x00, 0x00, 0x00, 0x0e, 0x85,
    ];
    let second_frame = vec![
        0xff, 0xf8, 0x64, 0x08, 0x01, 0x0f, 0xdb, 0x00, 0x00, 0x00, 0xf2, 0x80,
    ];
    let mut bytes = Vec::with_capacity(4 + 4 + stream_info.len() + 24);
    bytes.extend_from_slice(b"fLaC");
    bytes.extend_from_slice(&[0x80, 0, 0, 34]);
    bytes.extend_from_slice(&stream_info);
    bytes.extend_from_slice(&first_frame);
    bytes.extend_from_slice(&second_frame);
    AudioContainerFixture {
        container_id: "flac",
        extension: "flac",
        codec_id: "A_FLAC",
        bytes,
        codec_private: Some(stream_info),
        first_packet: first_frame,
        first_packet_duration_units: 16,
        sample_rate: 8_000,
        duration_is_known: true,
        streaming_duration_is_known: true,
    }
}

/// Три existing MPEG audio layers имеют отдельные exact codec identities.
#[derive(Clone, Copy)]
enum MpegAudioLayer {
    /// MPEG Audio Layer I.
    Layer1,
    /// MPEG Audio Layer II.
    Layer2,
    /// MPEG Audio Layer III.
    Layer3,
}

/// Генерирует два MPEG-1 audio frames с валидными layer-specific headers.
fn generated_mpeg_audio(layer: MpegAudioLayer) -> AudioContainerFixture {
    let (extension, codec_id, header_second_byte, bitrate_index, frame_bytes, duration_units) =
        match layer {
            MpegAudioLayer::Layer1 => ("mp1", "A_MP1", 0xff, 1_u8, 32_usize, 384_u64),
            MpegAudioLayer::Layer2 => ("mp2", "A_MP2", 0xfd, 8_u8, 417_usize, 1_152_u64),
            MpegAudioLayer::Layer3 => ("mp3", "A_MP3", 0xfb, 9_u8, 417_usize, 1_152_u64),
        };
    let header = [0xff, header_second_byte, bitrate_index << 4, 0xc0];
    let mut first_frame = vec![0_u8; frame_bytes];
    first_frame[..4].copy_from_slice(&header);
    let mut second_frame = first_frame.clone();
    second_frame[frame_bytes - 1] = 1;
    let mut bytes = Vec::with_capacity(frame_bytes * 2);
    bytes.extend_from_slice(&first_frame);
    bytes.extend_from_slice(&second_frame);
    AudioContainerFixture {
        container_id: "mpeg-audio",
        extension,
        codec_id,
        bytes,
        codec_private: None,
        first_packet: first_frame,
        first_packet_duration_units: duration_units,
        sample_rate: 44_100,
        duration_is_known: false,
        streaming_duration_is_known: false,
    }
}
