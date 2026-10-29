//! Какие контейнеры принимает источник и как их называет реестр демуксеров.
//!
//! Источник не угадывает формат по байтам: тот, кто приносит сегменты, заранее
//! объявляет тип полосы (у браузера это MIME из `addSourceBuffer`/`changeType`).
//! Реестр `demux-api` потом всё равно сверяет содержимое с объявленным контейнером
//! (`open_required_container`), поэтому подменить формат объявлением нельзя.

use demux_api::{DemuxContainerId, DemuxHints, DemuxIdentityError};

/// Контейнер одной полосы входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PushedContainer {
    /// Фрагментированный MP4 (ISO BMFF, CMAF): init `ftyp`+`moov`, медиа `moof`+`mdat`.
    FragmentedMp4,
    /// WebM (Matroska): init — EBML-заголовок и `Segment` до первого `Cluster`, медиа — `Cluster`.
    WebM,
    /// MPEG-TS: init-сегмента нет, таблицы PAT/PMT идут внутри потока.
    MpegTs,
}

/// Ошибка: объявленный тип полосы источник не умеет разбирать.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the media type `{essence}` is not supported by the pushed media source")]
pub struct UnsupportedMediaType {
    /// Только «суть» MIME без параметров (`video/x-flv`), обрезанная до разумной длины.
    essence: String,
}

impl UnsupportedMediaType {
    /// Суть отвергнутого MIME для диагностики.
    #[must_use]
    pub fn essence(&self) -> &str {
        &self.essence
    }
}

/// Сколько символов сути MIME сохраняем в ошибке: MIME приходит от чужой стороны,
/// и в журнал не должна попасть строка произвольной длины.
const MAX_REPORTED_ESSENCE_CHARS: usize = 64;

impl PushedContainer {
    /// Определяет контейнер по MIME вида `video/mp4; codecs="avc1.42c00a"`.
    ///
    /// Параметр `codecs` здесь не проверяется: поддержку кодека решают декодеры
    /// плеера, а источник отвечает только за контейнер.
    pub fn from_mime_type(mime_type: &str) -> Result<Self, UnsupportedMediaType> {
        let essence = mime_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match essence.as_str() {
            "video/mp4" | "audio/mp4" => Ok(Self::FragmentedMp4),
            "video/webm" | "audio/webm" => Ok(Self::WebM),
            "video/mp2t" => Ok(Self::MpegTs),
            _ => Err(UnsupportedMediaType {
                essence: essence.chars().take(MAX_REPORTED_ESSENCE_CHARS).collect(),
            }),
        }
    }

    /// Есть ли у контейнера отдельный init-сегмент, без которого медиа не разобрать.
    #[must_use]
    pub const fn has_initialization_segment(self) -> bool {
        match self {
            Self::FragmentedMp4 | Self::WebM => true,
            Self::MpegTs => false,
        }
    }

    /// Имя контейнера в реестре `demux-api` (регистрации `symphonia-demux` и `mpeg-ts-demux`).
    const fn registry_name(self) -> &'static str {
        match self {
            Self::FragmentedMp4 => "iso-bmff",
            Self::WebM => "webm",
            Self::MpegTs => "mpeg-ts",
        }
    }

    /// Точный идентификатор контейнера для `open_required_container`.
    pub(crate) fn demux_container_id(self) -> Result<DemuxContainerId, DemuxIdentityError> {
        DemuxContainerId::new(self.registry_name())
    }

    /// Подсказки реестру: тот же контейнер, что и требуемый, — без расширений и MIME.
    pub(crate) fn demux_hints(self) -> Result<DemuxHints, DemuxIdentityError> {
        Ok(DemuxHints::none().with_container(self.demux_container_id()?))
    }
}

#[cfg(test)]
mod tests {
    use super::PushedContainer;

    #[test]
    fn mime_essence_selects_container_regardless_of_codecs_and_case() {
        assert_eq!(
            PushedContainer::from_mime_type("video/mp4; codecs=\"avc1.42c00a\""),
            Ok(PushedContainer::FragmentedMp4)
        );
        assert_eq!(
            PushedContainer::from_mime_type(" Audio/MP4 ;codecs=mp4a.40.2"),
            Ok(PushedContainer::FragmentedMp4)
        );
        assert_eq!(
            PushedContainer::from_mime_type("video/webm; codecs=\"vp9,opus\""),
            Ok(PushedContainer::WebM)
        );
        assert_eq!(
            PushedContainer::from_mime_type("video/mp2t"),
            Ok(PushedContainer::MpegTs)
        );
    }

    #[test]
    fn unknown_mime_is_rejected_with_bounded_essence() {
        let long = format!("video/{}", "x".repeat(500));
        let error = PushedContainer::from_mime_type(&long).expect_err("unknown MIME");
        assert_eq!(error.essence().chars().count(), 64);
        let flv = PushedContainer::from_mime_type("video/x-flv").expect_err("FLV");
        assert_eq!(flv.essence(), "video/x-flv");
    }

    #[test]
    fn only_ts_lacks_initialization_segment() {
        assert!(PushedContainer::FragmentedMp4.has_initialization_segment());
        assert!(PushedContainer::WebM.has_initialization_segment());
        assert!(!PushedContainer::MpegTs.has_initialization_segment());
    }
}
