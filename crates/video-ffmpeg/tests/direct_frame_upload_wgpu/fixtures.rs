//! Каталог фикстур матрицы «кодек × битность × subsampling» и нарезка их на пакеты.
//!
//! Одна запись — одна клетка матрицы, которую играет программный путь плеера
//! (`codec_adapter::profile_accepts_format`). Файлы — `tests/fixtures/direct-frame-buffers/`
//! (`generate.sh`): 176x100, 12 кадров, ключевой кадр каждые 6.
//!
//! Нарезка без демуксера:
//! - IVF (VP8, VP9, AV1): 32-байтный заголовок файла, 12-байтный заголовок кадра;
//! - Annex B (H.264, HEVC): AUD перед каждым access unit, пакет = байты от AUD до AUD.

use std::path::PathBuf;

use bytes::Bytes;
use codec_core::{VideoCodec, VideoColorMetadata};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

const FIXTURE_DIRECTORY: &str = "tests/fixtures/direct-frame-buffers";

/// Как файл фикстуры режется на пакеты.
#[derive(Clone, Copy)]
pub enum FixtureContainer {
    /// IVF с этим кодом кодека в заголовке (`VP80`, `VP90`, `AV01`).
    Ivf { fourcc: [u8; 4] },
    /// Annex B с AUD перед каждым access unit.
    AnnexB { delimiter: AccessUnitDelimiter },
}

/// Какой NAL считается разделителем access unit-ов.
#[derive(Clone, Copy)]
pub enum AccessUnitDelimiter {
    /// H.264: NAL type 9 в младших 5 битах первого байта заголовка.
    H264,
    /// HEVC: NAL type 35 в битах 1..6 первого байта заголовка.
    Hevc,
}

impl AccessUnitDelimiter {
    fn matches(self, first_header_byte: u8) -> bool {
        match self {
            Self::H264 => first_header_byte & 0x1f == 9,
            Self::Hevc => (first_header_byte >> 1) & 0x3f == 35,
        }
    }
}

/// Одна клетка матрицы: кодек, файл и раскладка кадра, которую выбирает плеер.
pub struct FixtureStream {
    pub codec: VideoCodec,
    pub container: FixtureContainer,
    pub file_name: &'static str,
    pub contract: VideoFrameContract,
    /// Плану рисования 10/12 бит цвет обязателен (в сыром потоке его может не быть).
    pub resolved_color: Option<VideoColorMetadata>,
}

impl FixtureStream {
    fn new(
        codec: VideoCodec,
        container: FixtureContainer,
        file_name: &'static str,
        pixel_layout: VideoFramePixelLayout,
    ) -> Self {
        let eight_bit = matches!(
            pixel_layout,
            VideoFramePixelLayout::Yuv420Planar8
                | VideoFramePixelLayout::Yuv422Planar8
                | VideoFramePixelLayout::Yuv444Planar8
        );
        Self {
            codec,
            container,
            file_name,
            contract: VideoFrameContract {
                pixel_layout,
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            },
            resolved_color: (!eight_bit).then(VideoColorMetadata::sdr_bt709_limited),
        }
    }

    fn ivf(
        codec: VideoCodec,
        fourcc: [u8; 4],
        file_name: &'static str,
        pixel_layout: VideoFramePixelLayout,
    ) -> Self {
        Self::new(
            codec,
            FixtureContainer::Ivf { fourcc },
            file_name,
            pixel_layout,
        )
    }

    fn annex_b(
        codec: VideoCodec,
        delimiter: AccessUnitDelimiter,
        file_name: &'static str,
        pixel_layout: VideoFramePixelLayout,
    ) -> Self {
        Self::new(
            codec,
            FixtureContainer::AnnexB { delimiter },
            file_name,
            pixel_layout,
        )
    }

    // H.264: плеер принимает любой профиль только как 8 бит 4:2:0.
    pub fn h264() -> Self {
        Self::annex_b(
            VideoCodec::H264,
            AccessUnitDelimiter::H264,
            "h264-176x100-12frames.h264",
            VideoFramePixelLayout::Yuv420Planar8,
        )
    }

    // HEVC: Main, Main10, Main12, Main 4:2:2 10/12, Main 4:4:4 8/10.
    fn hevc(file_name: &'static str, pixel_layout: VideoFramePixelLayout) -> Self {
        Self::annex_b(
            VideoCodec::H265,
            AccessUnitDelimiter::Hevc,
            file_name,
            pixel_layout,
        )
    }

    pub fn hevc_main() -> Self {
        Self::hevc(
            "hevc-176x100-12frames.hevc",
            VideoFramePixelLayout::Yuv420Planar8,
        )
    }

    pub fn hevc_main10() -> Self {
        Self::hevc(
            "hevc-10bit-176x100-12frames.hevc",
            VideoFramePixelLayout::Yuv420Planar10Le,
        )
    }

    pub fn hevc_main12() -> Self {
        Self::hevc(
            "hevc-12bit-176x100-12frames.hevc",
            VideoFramePixelLayout::Yuv420Planar12Le,
        )
    }

    pub fn hevc_422_10bit() -> Self {
        Self::hevc(
            "hevc-422-10bit-176x100-12frames.hevc",
            VideoFramePixelLayout::Yuv422Planar10Le,
        )
    }

    pub fn hevc_422_12bit() -> Self {
        Self::hevc(
            "hevc-422-12bit-176x100-12frames.hevc",
            VideoFramePixelLayout::Yuv422Planar12Le,
        )
    }

    pub fn hevc_444() -> Self {
        Self::hevc(
            "hevc-444-176x100-12frames.hevc",
            VideoFramePixelLayout::Yuv444Planar8,
        )
    }

    pub fn hevc_444_10bit() -> Self {
        Self::hevc(
            "hevc-444-10bit-176x100-12frames.hevc",
            VideoFramePixelLayout::Yuv444Planar10Le,
        )
    }

    // VP8: только 8 бит 4:2:0.
    pub fn vp8() -> Self {
        Self::ivf(
            VideoCodec::Vp8,
            *b"VP80",
            "vp8-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv420Planar8,
        )
    }

    // VP9: профиль 0 (8/4:2:0), 1 (8/4:2:2, 4:4:4), 2 (10, 12/4:2:0), 3 (10, 12/4:2:2, 10/4:4:4).
    fn vp9_file(file_name: &'static str, pixel_layout: VideoFramePixelLayout) -> Self {
        Self::ivf(VideoCodec::Vp9, *b"VP90", file_name, pixel_layout)
    }

    pub fn vp9() -> Self {
        Self::vp9_file(
            "vp9-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv420Planar8,
        )
    }

    pub fn vp9_422() -> Self {
        Self::vp9_file(
            "vp9-422-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv422Planar8,
        )
    }

    pub fn vp9_444() -> Self {
        Self::vp9_file(
            "vp9-444-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv444Planar8,
        )
    }

    pub fn vp9_10bit() -> Self {
        Self::vp9_file(
            "vp9-10bit-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv420Planar10Le,
        )
    }

    pub fn vp9_12bit() -> Self {
        Self::vp9_file(
            "vp9-12bit-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv420Planar12Le,
        )
    }

    pub fn vp9_422_10bit() -> Self {
        Self::vp9_file(
            "vp9-422-10bit-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv422Planar10Le,
        )
    }

    pub fn vp9_422_12bit() -> Self {
        Self::vp9_file(
            "vp9-422-12bit-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv422Planar12Le,
        )
    }

    pub fn vp9_444_10bit() -> Self {
        Self::vp9_file(
            "vp9-444-10bit-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv444Planar10Le,
        )
    }

    // AV1: Main (8, 10/4:2:0) и High (8, 10/4:4:4); libdav1d с DR1 — патч `ffmpeg-vendor`.
    fn av1_file(file_name: &'static str, pixel_layout: VideoFramePixelLayout) -> Self {
        Self::ivf(VideoCodec::Av1, *b"AV01", file_name, pixel_layout)
    }

    pub fn av1() -> Self {
        Self::av1_file(
            "av1-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv420Planar8,
        )
    }

    pub fn av1_film_grain() -> Self {
        Self::av1_file(
            "av1-filmgrain-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv420Planar8,
        )
    }

    pub fn av1_10bit() -> Self {
        Self::av1_file(
            "av1-10bit-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv420Planar10Le,
        )
    }

    pub fn av1_444() -> Self {
        Self::av1_file(
            "av1-444-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv444Planar8,
        )
    }

    pub fn av1_444_10bit() -> Self {
        Self::av1_file(
            "av1-444-10bit-176x100-12frames.ivf",
            VideoFramePixelLayout::Yuv444Planar10Le,
        )
    }

    /// Ещё один экземпляр той же клетки (те же файл, контракт и цвет).
    pub fn same_cell(&self) -> Self {
        Self {
            codec: self.codec,
            container: self.container,
            file_name: self.file_name,
            contract: self.contract,
            resolved_color: self.resolved_color.clone(),
        }
    }

    /// Тот же поток без цвета в пакетах: как файл без цветовых тегов в контейнере.
    pub fn without_packet_color(self) -> Self {
        Self {
            resolved_color: None,
            ..self
        }
    }

    /// Пакеты фикстуры по порядку декодирования.
    pub fn read_packets(&self) -> Vec<Bytes> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(FIXTURE_DIRECTORY)
            .join(self.file_name);
        let file_bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
        match self.container {
            FixtureContainer::Ivf { fourcc } => ivf_packets(&file_bytes, fourcc, self.file_name),
            FixtureContainer::AnnexB { delimiter } => annex_b_access_units(&file_bytes, delimiter),
        }
    }
}

/// Режет IVF на пакеты-кадры и проверяет код кодека в заголовке.
fn ivf_packets(file_bytes: &[u8], fourcc: [u8; 4], file_name: &str) -> Vec<Bytes> {
    assert_eq!(&file_bytes[0..4], b"DKIF", "IVF signature of {file_name}");
    assert_eq!(&file_bytes[8..12], &fourcc, "IVF fourcc of {file_name}");
    let mut packets = Vec::new();
    let mut offset = 32;
    while offset + 12 <= file_bytes.len() {
        let frame_len =
            u32::from_le_bytes(file_bytes[offset..offset + 4].try_into().expect("4 bytes"))
                as usize;
        let payload_start = offset + 12;
        packets.push(Bytes::copy_from_slice(
            &file_bytes[payload_start..payload_start + frame_len],
        ));
        offset = payload_start + frame_len;
    }
    packets
}

/// Режет Annex B на access unit-ы: каждый начинается 4-байтным start code и AUD.
fn annex_b_access_units(file_bytes: &[u8], delimiter: AccessUnitDelimiter) -> Vec<Bytes> {
    let starts: Vec<usize> = (0..file_bytes.len().saturating_sub(5))
        .filter(|&index| {
            file_bytes[index..index + 4] == [0, 0, 0, 1] && delimiter.matches(file_bytes[index + 4])
        })
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(position, &start)| {
            let end = starts
                .get(position + 1)
                .copied()
                .unwrap_or(file_bytes.len());
            Bytes::copy_from_slice(&file_bytes[start..end])
        })
        .collect()
}
