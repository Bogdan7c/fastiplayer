//! Минимальный синтезатор ISO BMFF для тестов `elst`: даёт полный контроль над формой списка
//! правок, `ctts` и фрагментами, чего нельзя добиться от готового файла ffmpeg.

use std::io::Cursor;

use symphonia_core::formats::FormatOptions;
use symphonia_core::io::MediaSourceStream;

use crate::IsoMp4Reader;

/// Одна запись `elst`: `(segment_duration в movie timescale, media_time)`.
pub type EditEntry = (u64, i64);

/// Описание дорожки тестового файла.
#[derive(Clone)]
pub struct TrackSpec {
    pub id: u32,
    /// Media timescale (`mdhd`).
    pub timescale: u32,
    /// Записи `elst`; `None` — без `edts`.
    pub edits: Option<Vec<EditEntry>>,
    /// Использовать `elst` версии 1 (64-битные поля) вместо версии 0.
    pub edits_version_1: bool,
    /// Длительности samples в media timescale.
    pub durations: Vec<u32>,
    /// Composition offset каждого sample (для B-frame); пусто — без `ctts`.
    pub composition_offsets: Vec<i32>,
}

impl TrackSpec {
    pub fn audio(id: u32, timescale: u32, durations: Vec<u32>) -> Self {
        Self {
            id,
            timescale,
            edits: None,
            edits_version_1: false,
            durations,
            composition_offsets: Vec::new(),
        }
    }

    pub fn with_edits(mut self, edits: Vec<EditEntry>) -> Self {
        self.edits = Some(edits);
        self
    }

    pub fn with_edits_v1(mut self, edits: Vec<EditEntry>) -> Self {
        self.edits = Some(edits);
        self.edits_version_1 = true;
        self
    }

    pub fn with_composition_offsets(mut self, offsets: Vec<i32>) -> Self {
        self.composition_offsets = offsets;
        self
    }
}

/// Размер payload каждого sample: маркер `[track_id, sample_index, 0, 0]`.
pub const SAMPLE_SIZE: u32 = 4;
/// Movie timescale тестовых файлов (как у ffmpeg: 1000).
pub const MOVIE_TIMESCALE: u32 = 1000;

fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![version];
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    boxed(kind, &body)
}

fn be32(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

fn mvhd() -> Vec<u8> {
    let mut p = be32(&[0, 0, MOVIE_TIMESCALE, 0, 0x0001_0000]);
    p.extend_from_slice(&0x0100u16.to_be_bytes());
    p.extend_from_slice(&[0u8; 70]);
    full_box(b"mvhd", 0, 0, &p)
}

fn tkhd(track_id: u32) -> Vec<u8> {
    let mut p = be32(&[0, 0, track_id, 0, 0]);
    p.extend_from_slice(&[0u8; 8]);
    p.extend_from_slice(&[0u8; 4]);
    p.extend_from_slice(&0x0100u16.to_be_bytes());
    p.extend_from_slice(&[0u8; 2]);
    let matrix = [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
    p.extend_from_slice(&be32(&matrix));
    p.extend_from_slice(&be32(&[0, 0]));
    full_box(b"tkhd", 0, 3, &p)
}

fn elst(edits: &[EditEntry], version_1: bool) -> Vec<u8> {
    let mut p = be32(&[edits.len() as u32]);
    for (segment_duration, media_time) in edits {
        if version_1 {
            p.extend_from_slice(&segment_duration.to_be_bytes());
            p.extend_from_slice(&media_time.to_be_bytes());
        } else {
            p.extend_from_slice(&(*segment_duration as u32).to_be_bytes());
            p.extend_from_slice(&(*media_time as i32).to_be_bytes());
        }
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes());
    }
    boxed(b"edts", &full_box(b"elst", u8::from(version_1), 0, &p))
}

fn mdhd(timescale: u32) -> Vec<u8> {
    let mut p = be32(&[0, 0, timescale, 0]);
    p.extend_from_slice(&0x55c4u16.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    full_box(b"mdhd", 0, 0, &p)
}

fn hdlr() -> Vec<u8> {
    let mut p = be32(&[0]);
    p.extend_from_slice(b"soun");
    p.extend_from_slice(&[0u8; 12]);
    p.push(0);
    full_box(b"hdlr", 0, 0, &p)
}

/// Sample entry `alaw` (G.711): не PCM-coalescing, не требует codec-specific sub-box-ов.
fn stsd_alaw() -> Vec<u8> {
    let mut entry = vec![0u8; 6];
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0u8; 8]);
    entry.extend_from_slice(&1u16.to_be_bytes()); // channels
    entry.extend_from_slice(&16u16.to_be_bytes());
    entry.extend_from_slice(&[0u8; 4]);
    entry.extend_from_slice(&(8000u32 << 16).to_be_bytes());
    full_box(
        b"stsd",
        0,
        0,
        &[&be32(&[1])[..], &boxed(b"alaw", &entry)[..]].concat(),
    )
}

fn ctts(offsets: &[i32]) -> Vec<u8> {
    let mut p = be32(&[offsets.len() as u32]);
    for offset in offsets {
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&offset.to_be_bytes());
    }
    full_box(b"ctts", 1, 0, &p)
}

fn trak(spec: &TrackSpec, chunk_offset: u32, classic: bool) -> Vec<u8> {
    let count = if classic {
        spec.durations.len() as u32
    } else {
        0
    };
    let stts = if classic {
        let mut p = be32(&[count]);
        for duration in &spec.durations {
            p.extend_from_slice(&be32(&[1, *duration]));
        }
        full_box(b"stts", 0, 0, &p)
    } else {
        full_box(b"stts", 0, 0, &be32(&[0]))
    };
    let stsc = full_box(
        b"stsc",
        0,
        0,
        &if classic {
            be32(&[1, 1, count, 1])
        } else {
            be32(&[0])
        },
    );
    let stsz = full_box(
        b"stsz",
        0,
        0,
        &if classic {
            be32(&[SAMPLE_SIZE, count])
        } else {
            be32(&[0, 0])
        },
    );
    let stco = full_box(
        b"stco",
        0,
        0,
        &if classic {
            be32(&[1, chunk_offset])
        } else {
            be32(&[0])
        },
    );
    let mut stbl = stsd_alaw();
    stbl.extend(stts);
    if classic && !spec.composition_offsets.is_empty() {
        stbl.extend(ctts(&spec.composition_offsets));
    }
    stbl.extend(stsc);
    stbl.extend(stsz);
    stbl.extend(stco);
    let smhd = full_box(b"smhd", 0, 0, &be32(&[0]));
    let minf = boxed(b"minf", &[smhd, boxed(b"stbl", &stbl)].concat());
    let mdia = boxed(b"mdia", &[mdhd(spec.timescale), hdlr(), minf].concat());
    let mut body = tkhd(spec.id);
    if let Some(edits) = &spec.edits {
        body.extend(elst(edits, spec.edits_version_1));
    }
    body.extend(mdia);
    boxed(b"trak", &body)
}

fn ftyp() -> Vec<u8> {
    boxed(
        b"ftyp",
        &[b"isom".as_slice(), &be32(&[0]), b"isom"].concat(),
    )
}

fn sample_payload(track_id: u32, index: usize) -> Vec<u8> {
    vec![track_id as u8, index as u8, 0, 0]
}

/// Классический (progressive) MP4: `ftyp, moov, mdat`.
pub fn classic_mp4(tracks: &[TrackSpec]) -> Vec<u8> {
    let build_moov = |offsets: &[u32]| {
        let traks: Vec<u8> = tracks
            .iter()
            .zip(offsets)
            .flat_map(|(spec, offset)| trak(spec, *offset, true))
            .collect();
        boxed(b"moov", &[mvhd(), traks].concat())
    };
    let ftyp = ftyp();
    let moov_len = build_moov(&vec![0; tracks.len()]).len();
    let mut offset = (ftyp.len() + moov_len + 8) as u32;
    let mut offsets = Vec::new();
    let mut mdat = Vec::new();
    for spec in tracks {
        offsets.push(offset);
        for index in 0..spec.durations.len() {
            mdat.extend(sample_payload(spec.id, index));
        }
        offset += SAMPLE_SIZE * spec.durations.len() as u32;
    }
    [ftyp, build_moov(&offsets), boxed(b"mdat", &mdat)].concat()
}

/// Один фрагмент: samples одной дорожки с заданным `tfdt`.
pub struct FragmentSpec {
    pub track_id: u32,
    pub base_decode_time: u64,
    pub durations: Vec<u32>,
}

/// Фрагментированный MP4: init (`ftyp, moov+mvex`) и последовательность `moof+mdat`.
pub fn fragmented_mp4(tracks: &[TrackSpec], fragments: &[FragmentSpec]) -> Vec<u8> {
    let traks: Vec<u8> = tracks
        .iter()
        .flat_map(|spec| trak(spec, 0, false))
        .collect();
    let trexs: Vec<u8> = tracks
        .iter()
        .flat_map(|spec| full_box(b"trex", 0, 0, &be32(&[spec.id, 1, 0, 0, 0])))
        .collect();
    let mut out = ftyp();
    out.extend(boxed(
        b"moov",
        &[mvhd(), traks, boxed(b"mvex", &trexs)].concat(),
    ));
    for (sequence, fragment) in fragments.iter().enumerate() {
        let sample_count = fragment.durations.len() as u32;
        let build_moof = |data_offset: u32| {
            let mfhd = full_box(b"mfhd", 0, 0, &be32(&[sequence as u32 + 1]));
            // default-base-is-moof
            let tfhd = full_box(b"tfhd", 0, 0x2_0000, &be32(&[fragment.track_id]));
            let mut tfdt_payload = fragment.base_decode_time.to_be_bytes().to_vec();
            tfdt_payload.shrink_to_fit();
            let tfdt = full_box(b"tfdt", 1, 0, &tfdt_payload);
            // data-offset | sample-duration | sample-size
            let mut run = be32(&[sample_count, data_offset]);
            for duration in &fragment.durations {
                run.extend(be32(&[*duration, SAMPLE_SIZE]));
            }
            let trun = full_box(b"trun", 0, 0x1 | 0x100 | 0x200, &run);
            boxed(
                b"moof",
                &[mfhd, boxed(b"traf", &[tfhd, tfdt, trun].concat())].concat(),
            )
        };
        let moof_len = build_moof(0).len() as u32;
        out.extend(build_moof(moof_len + 8));
        let payload: Vec<u8> = (0..fragment.durations.len())
            .flat_map(|index| sample_payload(fragment.track_id, index))
            .collect();
        out.extend(boxed(b"mdat", &payload));
    }
    out
}

/// Открывает синтезированный файл через production-конструктор reader-а.
pub fn open(bytes: Vec<u8>) -> IsoMp4Reader<'static> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    match IsoMp4Reader::try_new_from_stream_start(source, FormatOptions::default()) {
        Ok(reader) => reader,
        Err(error) => panic!("синтезированный MP4 должен открываться: {error}"),
    }
}
