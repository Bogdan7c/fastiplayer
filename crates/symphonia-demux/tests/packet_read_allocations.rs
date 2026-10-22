// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Горячий путь чтения packet-ов `SymphoniaDemuxer` не делает собственных heap allocation-ов.
//!
//! Профиль `player-worker` (cpu-parity 06d) показал на каждом packet-е лишнюю работу:
//! eager-построение `anyhow::Error` в `format_mut` (дважды на packet) и clone + сравнение
//! `MediaMetadata`, даже когда reader не приносил новой metadata revision.
//!
//! Тест сравнивает число allocation-ов при чтении одного и того же входа двумя путями:
//! напрямую через Symphonia `FormatReader::next_packet` (эталон: то, что неизбежно тратит
//! сам reader на packet) и через `SymphoniaDemuxer::next_event`. Сверх эталона demuxer
//! не должен добавлять ни одного allocation-а на packet. Эталон считается на той же
//! сборке Symphonia, поэтому тест не зависит от того, сколько аллоцирует сам reader.
//!
//! Глобальный allocator считает только в потоке, где включён замер, — параллельные тесты
//! этого же бинарника на счётчик не влияют.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use media_core::{DemuxReadEvent, Demuxer};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia_demux::SymphoniaDemuxer;

#[global_allocator]
static COUNTING_ALLOCATOR: CountingAllocator = CountingAllocator;

/// Сколько allocation-ов на packet demuxer может добавить к эталону Symphonia.
/// Конвертация packet-а (`Box<[u8]>` -> `Bytes`) payload не копирует, metadata без
/// новых revisions не трогается, поэтому допуск — ноль.
const ALLOWED_DEMUXER_ALLOCATIONS_PER_PACKET: u64 = 0;

/// Название, которое генерируемый WAV несёт в ID3v2 `TIT2` перед RIFF-заголовком.
const STATIC_TITLE: &str = "Static header title";

/// Параметры генерируемого PCM WAV: 1 с моно 16 бит — несколько десятков packet-ов.
const WAV_SAMPLE_RATE_HZ: u32 = 48_000;
const WAV_FRAME_COUNT: u32 = 48_000;
const WAV_BITS_PER_SAMPLE: u16 = 16;

/// Минимум packet-ов, чтобы per-packet лишняя работа гарантированно была заметна.
const MIN_MEASURED_PACKETS: u64 = 10;

struct CountingAllocator;

thread_local! {
    static ALLOCATION_PROBE: AllocationProbe = const { AllocationProbe::new() };
}

/// Счётчик allocation-ов текущего потока; включается только на время замера.
struct AllocationProbe {
    enabled: Cell<bool>,
    allocations: Cell<u64>,
}

impl AllocationProbe {
    const fn new() -> Self {
        Self {
            enabled: Cell::new(false),
            allocations: Cell::new(0),
        }
    }

    fn record_allocation(&self) {
        if self.enabled.get() {
            self.allocations
                .set(self.allocations.get().saturating_add(1));
        }
    }
}

// SAFETY: все вызовы делегируются `System` без изменения layout/pointer;
// thread-local счётчик const-инициализирован и сам ничего не аллоцирует.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        record_allocation();
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        record_allocation();
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        record_allocation();
        new_pointer
    }
}

/// Учитывает allocation, если в этом потоке идёт замер.
fn record_allocation() {
    // `try_with`: при завершении потока thread-local уже может быть уничтожен,
    // а allocator обязан продолжать работать — такой allocation просто не считается.
    let _probe_alive = ALLOCATION_PROBE.try_with(AllocationProbe::record_allocation);
}

/// Результат замера одного пути чтения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadMeasurement {
    packets: u64,
    allocations: u64,
}

/// Считает allocation-ы, сделанные `read_all` в текущем потоке.
fn count_allocations(read_all: impl FnOnce() -> u64) -> ReadMeasurement {
    ALLOCATION_PROBE.with(|probe| {
        probe.allocations.set(0);
        probe.enabled.set(true);
    });
    let packets = read_all();
    let allocations = ALLOCATION_PROBE.with(|probe| {
        probe.enabled.set(false);
        probe.allocations.get()
    });
    ReadMeasurement {
        packets,
        allocations,
    }
}

/// Эталон: читает все packet-ы напрямую из Symphonia reader-а.
fn measure_raw_symphonia(mut reader: Box<dyn FormatReader + '_>) -> ReadMeasurement {
    count_allocations(|| {
        let mut packets = 0;
        while let Some(packet) = reader
            .next_packet()
            .expect("raw Symphonia reader must read the fixture")
        {
            drop(packet);
            packets += 1;
        }
        packets
    })
}

/// Проверяемый путь: читает все events demuxer-а до EOF; metadata-событий быть не должно.
fn measure_demuxer(demuxer: &mut SymphoniaDemuxer) -> ReadMeasurement {
    count_allocations(|| {
        let mut packets = 0;
        loop {
            match demuxer.next_event().expect("demuxer must read the fixture") {
                DemuxReadEvent::Packet(packet) => {
                    drop(packet);
                    packets += 1;
                }
                DemuxReadEvent::EndOfStream => break,
                unexpected => panic!("static input must yield only packets, got {unexpected:?}"),
            }
        }
        packets
    })
}

/// Сверяет demuxer с эталоном: те же packet-ы и ни одного лишнего allocation-а на packet.
fn assert_no_demuxer_allocations_per_packet(raw: ReadMeasurement, demuxer: ReadMeasurement) {
    assert!(
        raw.packets >= MIN_MEASURED_PACKETS,
        "fixture is too short to expose per-packet work: {raw:?}"
    );
    assert_eq!(
        demuxer.packets, raw.packets,
        "demuxer must surface every container packet"
    );
    let allowed = raw.allocations + raw.packets * ALLOWED_DEMUXER_ALLOCATIONS_PER_PACKET;
    assert!(
        demuxer.allocations <= allowed,
        "demuxer adds per-packet heap work on top of Symphonia: raw={raw:?}, demuxer={demuxer:?}"
    );
}

fn fixture(relative_path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative_path)
}

/// Открывает файл напрямую через Symphonia probe (тот же patched reader, что у demuxer-а).
fn probe_raw_file(path: &Path, extension: &str) -> Box<dyn FormatReader + 'static> {
    let file = File::open(path).expect("fixture must exist");
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    probe_raw(stream, extension)
}

fn probe_raw(stream: MediaSourceStream<'static>, extension: &str) -> Box<dyn FormatReader> {
    let mut hint = Hint::new();
    hint.with_extension(extension);
    symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("raw Symphonia probe must open the fixture")
}

/// Реальный файл из репозитория: каждый packet раньше строил и ронял `anyhow::Error`.
#[test]
fn audio_packets_are_read_without_demuxer_allocations() {
    let path = fixture("ac3/audio-only-eac3-5p1.m4a");
    let raw = measure_raw_symphonia(probe_raw_file(&path, "m4a"));

    let mut demuxer = SymphoniaDemuxer::from_file(&path).expect("demuxer must open the fixture");
    let measured = measure_demuxer(&mut demuxer);

    assert_no_demuxer_allocations_per_packet(raw, measured);
}

/// Video packet-ы: `packet_mapper` определяет codec для проверки keyframe на каждом
/// packet-е; раньше `codec_core::VideoCodec::from_container_codec_id` делал для этого
/// `to_ascii_uppercase` — одна аллокация на video packet (cpu-parity 06d).
#[test]
fn video_packets_are_read_without_demuxer_allocations() {
    let path = fixture("matroska/chapters-without-edition-uid.mkv");
    let raw = measure_raw_symphonia(probe_raw_file(&path, "mkv"));

    let mut demuxer = SymphoniaDemuxer::from_file(&path).expect("demuxer must open the fixture");
    let measured = measure_demuxer(&mut demuxer);

    assert_no_demuxer_allocations_per_packet(raw, measured);
}

/// WAV со статичным названием в ID3v2: metadata есть, но новых revisions по ходу
/// чтения нет — demuxer не должен клонировать/пересобирать её на каждом packet-е.
#[test]
fn static_metadata_is_not_rebuilt_per_packet() {
    let wav_bytes = id3_tagged_pcm_wav(STATIC_TITLE);
    let raw_stream = MediaSourceStream::new(
        Box::new(ReadOnlySource::new(Cursor::new(wav_bytes.clone()))),
        Default::default(),
    );
    let raw = measure_raw_symphonia(probe_raw(raw_stream, "wav"));

    let mut demuxer = SymphoniaDemuxer::from_stream(Cursor::new(wav_bytes), "wav", "id3-wav")
        .expect("demuxer must open the generated WAV");
    let open_metadata = demuxer
        .media_metadata()
        .expect("Symphonia demuxer always publishes metadata");
    assert_eq!(
        open_metadata.tags.title.as_deref(),
        Some(STATIC_TITLE),
        "fixture must really carry format-level metadata"
    );

    let measured = measure_demuxer(&mut demuxer);

    assert_no_demuxer_allocations_per_packet(raw, measured);
    assert_eq!(
        demuxer.media_metadata(),
        Some(open_metadata),
        "static metadata must survive packet reading unchanged"
    );
}

/// Собирает ID3v2.4 тег с `TIT2` и следом PCM WAV (тишина), как пишут некоторые теггеры.
fn id3_tagged_pcm_wav(title: &str) -> Vec<u8> {
    let mut bytes = id3v2_title_tag(title);
    bytes.extend_from_slice(&pcm_wav_silence());
    bytes
}

/// ID3v2.4: заголовок `ID3` + один текстовый frame `TIT2` в UTF-8.
fn id3v2_title_tag(title: &str) -> Vec<u8> {
    const UTF8_TEXT_ENCODING: u8 = 0x03;
    let mut frame_body = vec![UTF8_TEXT_ENCODING];
    frame_body.extend_from_slice(title.as_bytes());

    let mut frame = b"TIT2".to_vec();
    frame.extend_from_slice(&id3v2_syncsafe_size(frame_body.len()));
    frame.extend_from_slice(&[0, 0]); // flags frame-а
    frame.extend_from_slice(&frame_body);

    let mut tag = b"ID3".to_vec();
    tag.extend_from_slice(&[4, 0]); // версия 2.4.0
    tag.push(0); // flags тега
    tag.extend_from_slice(&id3v2_syncsafe_size(frame.len()));
    tag.extend_from_slice(&frame);
    tag
}

/// ID3v2 хранит размеры как 4 байта по 7 значащих бит (syncsafe integer).
fn id3v2_syncsafe_size(size: usize) -> [u8; 4] {
    let size = u32::try_from(size).expect("test tag is tiny");
    [
        ((size >> 21) & 0x7f) as u8,
        ((size >> 14) & 0x7f) as u8,
        ((size >> 7) & 0x7f) as u8,
        (size & 0x7f) as u8,
    ]
}

/// Канонический RIFF/WAVE: `fmt ` (PCM, моно) + `data` с тишиной.
fn pcm_wav_silence() -> Vec<u8> {
    const PCM_FORMAT_TAG: u16 = 1;
    const CHANNELS: u16 = 1;
    let block_align = CHANNELS * WAV_BITS_PER_SAMPLE / 8;
    let byte_rate = WAV_SAMPLE_RATE_HZ * u32::from(block_align);
    let data_len = WAV_FRAME_COUNT * u32::from(block_align);

    let mut fmt_chunk = Vec::new();
    fmt_chunk.extend_from_slice(&PCM_FORMAT_TAG.to_le_bytes());
    fmt_chunk.extend_from_slice(&CHANNELS.to_le_bytes());
    fmt_chunk.extend_from_slice(&WAV_SAMPLE_RATE_HZ.to_le_bytes());
    fmt_chunk.extend_from_slice(&byte_rate.to_le_bytes());
    fmt_chunk.extend_from_slice(&block_align.to_le_bytes());
    fmt_chunk.extend_from_slice(&WAV_BITS_PER_SAMPLE.to_le_bytes());

    let mut wave_body = b"WAVE".to_vec();
    wave_body.extend_from_slice(b"fmt ");
    wave_body.extend_from_slice(&chunk_len(fmt_chunk.len()).to_le_bytes());
    wave_body.extend_from_slice(&fmt_chunk);
    wave_body.extend_from_slice(b"data");
    wave_body.extend_from_slice(&data_len.to_le_bytes());
    wave_body.resize(wave_body.len() + data_len as usize, 0);

    let mut riff = b"RIFF".to_vec();
    riff.extend_from_slice(&chunk_len(wave_body.len()).to_le_bytes());
    riff.extend_from_slice(&wave_body);
    riff
}

fn chunk_len(len: usize) -> u32 {
    u32::try_from(len).expect("test RIFF chunk fits u32")
}
