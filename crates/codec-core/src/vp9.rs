use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, ColorMetadataOrigin, ColorPrimaries, ColorRange,
    MatrixCoefficients, TransferFunction, VideoCodec, VideoColorMetadata, VideoDecodeRequirement,
    VideoProfile, Vp9Profile,
};

/// Результат VP9 packet/header probing для capability layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9RequirementProbe {
    /// Header надёжно распознан и дал requirement, который можно сверять с backend/render matrix.
    Candidate(Vp9RequirementCandidate),

    /// Header надёжно распознан, но вариант VP9 выходит за production policy Phase 9.
    Rejected(Vp9RequirementRejection),

    /// Header нельзя использовать для строгого отказа: decoder должен получить packet и продолжить.
    Recoverable(Vp9RequirementUncertainty),
}

/// Поддержанный или потенциально поддержанный VP9 requirement из надёжного header-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Vp9RequirementCandidate {
    /// Нормализованное требование к hardware decoder-у.
    pub requirement: VideoDecodeRequirement,

    /// Ожидаемый decoded surface format на границе decoder/renderer.
    pub decoded_format: Vp9DecodedFormatRequirement,

    /// Color metadata, которую VP9 bitstream header смог выразить напрямую.
    pub bitstream_color: Option<VideoColorMetadata>,
}

/// Ожидаемый decoded format для VP9 variant-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9DecodedFormatRequirement {
    /// 8-bit 4:2:0 path, который renderer уже принимает как NV12.
    Nv12,

    /// 10-bit 4:2:0 path, который Phase 9 распознаёт как P010 boundary candidate.
    P010,
}

/// Один источник metadata, участвующий в VP9 resolver-е.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Vp9MetadataSource {
    /// Источник этих hints: manifest, container, bitstream или backend.
    pub origin: ColorMetadataOrigin,

    /// Profile hint из manifest/container.
    pub profile: Option<VideoProfile>,

    /// Bit depth hint из manifest/container.
    pub bit_depth: Option<BitDepth>,

    /// Chroma hint из manifest/container.
    pub chroma: Option<ChromaSubsampling>,

    /// Coded width hint.
    pub width: Option<u32>,

    /// Coded height hint.
    pub height: Option<u32>,

    /// Color metadata hint.
    pub color: Option<VideoColorMetadata>,
}

impl Vp9MetadataSource {
    /// Создаёт пустой source с заданным origin для test fixtures и adapters.
    #[must_use]
    pub const fn empty(origin: ColorMetadataOrigin) -> Self {
        Self {
            origin,
            profile: None,
            bit_depth: None,
            chroma: None,
            width: None,
            height: None,
            color: None,
        }
    }

    /// Создаёт source из container-level track metadata.
    #[must_use]
    pub const fn container() -> Self {
        Self::empty(ColorMetadataOrigin::Container)
    }

    /// Возвращает source с profile hint.
    #[must_use]
    pub const fn with_profile(mut self, profile: VideoProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Возвращает source с bit depth hint.
    #[must_use]
    pub const fn with_bit_depth(mut self, bit_depth: BitDepth) -> Self {
        self.bit_depth = Some(bit_depth);
        self
    }

    /// Возвращает source с chroma hint.
    #[must_use]
    pub const fn with_chroma(mut self, chroma: ChromaSubsampling) -> Self {
        self.chroma = Some(chroma);
        self
    }

    /// Возвращает source с resolution hint.
    #[must_use]
    pub const fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Возвращает source с color metadata.
    #[must_use]
    pub fn with_color(mut self, color: VideoColorMetadata) -> Self {
        self.color = Some(color);
        self
    }
}

/// Результат VP9 metadata resolver-а после field-level precedence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Vp9ResolvedMetadata {
    /// Итоговое decode requirement после слияния container и bitstream полей.
    pub requirement: VideoDecodeRequirement,

    /// Ожидаемый decoded format, если bitstream уже дал достаточно данных.
    pub decoded_format: Option<Vp9DecodedFormatRequirement>,

    /// Итоговая color metadata для diagnostics/render boundary.
    pub color: VideoColorMetadata,

    /// Typed diagnostics по конфликтам и fallback-ам.
    pub diagnostics: Vec<Vp9MetadataDiagnostic>,
}

/// Typed diagnostic от VP9 metadata resolver-а.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9MetadataDiagnostic {
    /// Более приоритетный source заменил поле из менее приоритетного source-а.
    FieldConflict(Vp9MetadataConflict),

    /// SDR fallback применён только потому, что media metadata не дала colorimetry.
    SdrFallbackApplied,
}

/// Поле, для которого resolver умеет фиксировать conflict/missing diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9MetadataField {
    /// Codec profile.
    Profile,

    /// Bit depth.
    BitDepth,

    /// Chroma subsampling.
    Chroma,

    /// Coded resolution.
    Resolution,

    /// Color range.
    Range,

    /// Matrix coefficients.
    Matrix,

    /// Color primaries.
    Primaries,

    /// Transfer function.
    Transfer,

    /// Decoded format at decoder/render boundary.
    DecodedFormat,
}

/// Один конфликт между двумя metadata источниками.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Vp9MetadataConflict {
    /// Поле, в котором обнаружено расхождение.
    pub field: Vp9MetadataField,

    /// Источник выбранного значения.
    pub selected_origin: ColorMetadataOrigin,

    /// Выбранное значение в стабильной diagnostic форме.
    pub selected_value: String,

    /// Источник отклонённого значения.
    pub rejected_origin: ColorMetadataOrigin,

    /// Отклонённое значение в стабильной diagnostic форме.
    pub rejected_value: String,
}

/// Ошибка strict HDR core validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9StrictHdrValidationError {
    /// Обязательное HDR поле отсутствует или осталось `Unknown`.
    MissingField(Vp9MetadataField),

    /// Поле есть, но его значение не входит в strict HDR core contract.
    InvalidField {
        /// Поле, которое нарушает strict contract.
        field: Vp9MetadataField,

        /// Ожидаемое значение в diagnostic форме.
        expected: String,

        /// Фактическое значение в diagnostic форме.
        actual: String,
    },
}

/// Строгий VP9 policy reject, полученный из валидного bitstream header-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9RequirementRejection {
    /// VP9 12-bit требует P012/12-bit path, которого нет в scope Phase 9.
    UnsupportedBitDepth(u8),

    /// VP9 4:2:2/4:4:4 не поддерживается текущим production renderer/decode contract.
    UnsupportedChroma(ChromaSubsampling),
}

impl Vp9RequirementRejection {
    /// Возвращает user-facing diagnostic без привязки к конкретному hardware backend-у.
    #[must_use]
    pub fn user_message(self) -> String {
        match self {
            Self::UnsupportedBitDepth(bit_depth) => {
                format!("VP9 {bit_depth}-bit не поддерживается: P012/12-bit path вне scope Phase 9")
            }
            Self::UnsupportedChroma(chroma) => format!(
                "VP9 chroma/profile combination {chroma} не поддерживается: production path принимает только VP9 Profile 0/2 4:2:0"
            ),
        }
    }
}

/// Причина, по которой VP9 packet нельзя использовать для строгого capability reject-а.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9RequirementUncertainty {
    /// Parser не смог надёжно разобрать header; это diagnostic, а не hardware reject.
    ParseError(String),

    /// Packet только показывает уже декодированный кадр и не несёт stream requirement.
    ShowExistingFrame,

    /// Inter/non-keyframe не содержит color config, поэтому profile/format ещё нельзя фиксировать.
    MissingColorConfig {
        /// Profile уже прочитан из packet header-а.
        profile: Vp9Profile,
    },

    /// Header содержит chroma flag combination, которой нет в текущей typed model.
    UnknownChromaLayout {
        /// VP9 subsampling_x flag из header-а.
        subsampling_x: bool,

        /// VP9 subsampling_y flag из header-а.
        subsampling_y: bool,
    },

    /// Header содержит bit depth вне известной VP9 matrix.
    UnknownBitDepth {
        /// Сырое значение bit depth из parser-а.
        bit_depth: u8,
    },
}

/// Читает VP9 uncompressed header и строит Phase 9 requirement decision.
#[must_use]
pub fn probe_vp9_packet_requirement(packet_bytes: &[u8]) -> Vp9RequirementProbe {
    match vp9_parser::parse_uncompressed_header(packet_bytes) {
        Ok(frame_info) => requirement_probe_from_frame_info(frame_info),
        Err(error) => Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ParseError(
            error.to_string(),
        )),
    }
}

/// Объединяет container hints и confirmed VP9 bitstream metadata по field-level precedence.
#[must_use]
pub fn resolve_vp9_metadata(
    container_source: Option<Vp9MetadataSource>,
    bitstream_candidate: Option<Vp9RequirementCandidate>,
) -> Vp9ResolvedMetadata {
    let mut diagnostics = Vec::new();
    let mut requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);

    if let Some(container) = &container_source {
        apply_container_requirement_hints(&mut requirement, container);
    }

    if let Some(candidate) = &bitstream_candidate {
        apply_bitstream_requirement(
            &mut requirement,
            candidate,
            &container_source,
            &mut diagnostics,
        );
    }

    let color = resolve_vp9_color_metadata(
        container_source
            .as_ref()
            .and_then(|source| source.color.as_ref()),
        bitstream_candidate
            .as_ref()
            .and_then(|candidate| candidate.bitstream_color.as_ref()),
        &mut diagnostics,
    );
    requirement = requirement.with_color(color.clone());

    Vp9ResolvedMetadata {
        requirement,
        decoded_format: bitstream_candidate.map(|candidate| candidate.decoded_format),
        color,
        diagnostics,
    }
}

/// Проверяет strict HDR core metadata contract для VP9 Profile 2/P010 boundary.
pub fn validate_vp9_strict_hdr_core(
    resolved_metadata: &Vp9ResolvedMetadata,
) -> Result<(), Vp9StrictHdrValidationError> {
    let requirement = &resolved_metadata.requirement;

    validate_required_value(
        Vp9MetadataField::Profile,
        requirement.profile.map(|profile| profile.to_string()),
        Some(VideoProfile::Vp9(Vp9Profile::Profile2).to_string()),
    )?;
    validate_required_value(
        Vp9MetadataField::BitDepth,
        requirement.bit_depth.map(|bit_depth| bit_depth.to_string()),
        Some(BitDepth::Ten.to_string()),
    )?;
    validate_required_value(
        Vp9MetadataField::Chroma,
        requirement.chroma.map(|chroma| chroma.to_string()),
        Some(ChromaSubsampling::Yuv420.to_string()),
    )?;
    validate_required_value(
        Vp9MetadataField::DecodedFormat,
        resolved_metadata
            .decoded_format
            .map(|decoded_format| decoded_format.display_name().to_string()),
        Some(Vp9DecodedFormatRequirement::P010.display_name().to_string()),
    )?;
    validate_required_value(
        Vp9MetadataField::Primaries,
        known_primaries_name(resolved_metadata.color.primaries),
        Some("BT.2020".to_string()),
    )?;
    validate_required_value(
        Vp9MetadataField::Matrix,
        known_matrix_name(resolved_metadata.color.matrix),
        Some("BT.2020".to_string()),
    )?;
    validate_required_value(
        Vp9MetadataField::Range,
        known_range_name(resolved_metadata.color.range),
        None,
    )?;

    let Some(transfer_name) = known_transfer_name(resolved_metadata.color.transfer) else {
        return Err(Vp9StrictHdrValidationError::MissingField(
            Vp9MetadataField::Transfer,
        ));
    };

    if matches!(
        resolved_metadata.color.transfer,
        TransferFunction::Pq | TransferFunction::Hlg
    ) {
        Ok(())
    } else {
        Err(Vp9StrictHdrValidationError::InvalidField {
            field: Vp9MetadataField::Transfer,
            expected: "PQ или HLG".to_string(),
            actual: transfer_name,
        })
    }
}

/// Переводит уже распарсенный VP9 header в candidate/reject/recoverable model.
fn requirement_probe_from_frame_info(frame_info: vp9_parser::Vp9FrameInfo) -> Vp9RequirementProbe {
    if frame_info.show_existing_frame {
        return Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ShowExistingFrame);
    }

    let Some(profile) = Vp9Profile::from_bitstream_value(frame_info.profile) else {
        return Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ParseError(format!(
            "unsupported VP9 profile {}",
            frame_info.profile
        )));
    };

    let Some(bit_depth) = frame_info.bit_depth else {
        return Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::MissingColorConfig {
            profile,
        });
    };

    let Some(chroma) =
        chroma_from_subsampling_flags(frame_info.subsampling_x, frame_info.subsampling_y)
    else {
        return recoverable_chroma_uncertainty(frame_info.subsampling_x, frame_info.subsampling_y);
    };

    if matches!(profile, Vp9Profile::Profile1 | Vp9Profile::Profile3)
        || matches!(
            chroma,
            ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444
        )
    {
        return Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedChroma(chroma));
    }

    let Some(bit_depth_model) = bit_depth_from_header(bit_depth) else {
        return recoverable_bit_depth_uncertainty(bit_depth);
    };

    let Some(decoded_format) = decoded_format_from_supported_bit_depth(bit_depth_model) else {
        return Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedBitDepth(
            bit_depth,
        ));
    };

    let mut requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(profile))
        .with_bit_depth(bit_depth_model)
        .with_chroma(chroma);

    if frame_info.width > 0 && frame_info.height > 0 {
        requirement = requirement.with_resolution(frame_info.width, frame_info.height);
    }

    Vp9RequirementProbe::Candidate(Vp9RequirementCandidate {
        requirement,
        decoded_format,
        bitstream_color: bitstream_color_from_frame_info(&frame_info),
    })
}

/// Применяет ранние container hints без strict override логики.
fn apply_container_requirement_hints(
    requirement: &mut VideoDecodeRequirement,
    container_source: &Vp9MetadataSource,
) {
    requirement.profile = container_source.profile;
    requirement.bit_depth = container_source.bit_depth;
    requirement.chroma = container_source.chroma;
    requirement.width = container_source.width;
    requirement.height = container_source.height;
}

/// Применяет confirmed bitstream fields и фиксирует conflicts с container hints.
fn apply_bitstream_requirement(
    requirement: &mut VideoDecodeRequirement,
    bitstream_candidate: &Vp9RequirementCandidate,
    container_source: &Option<Vp9MetadataSource>,
    diagnostics: &mut Vec<Vp9MetadataDiagnostic>,
) {
    record_requirement_conflict(
        Vp9MetadataField::Profile,
        container_source.as_ref().and_then(|source| source.profile),
        bitstream_candidate.requirement.profile,
        diagnostics,
    );
    record_requirement_conflict(
        Vp9MetadataField::BitDepth,
        container_source
            .as_ref()
            .and_then(|source| source.bit_depth),
        bitstream_candidate.requirement.bit_depth,
        diagnostics,
    );
    record_requirement_conflict(
        Vp9MetadataField::Chroma,
        container_source.as_ref().and_then(|source| source.chroma),
        bitstream_candidate.requirement.chroma,
        diagnostics,
    );
    record_resolution_conflict(container_source, bitstream_candidate, diagnostics);

    requirement.profile = bitstream_candidate.requirement.profile;
    requirement.bit_depth = bitstream_candidate.requirement.bit_depth;
    requirement.chroma = bitstream_candidate.requirement.chroma;
    requirement.width = bitstream_candidate.requirement.width.or(requirement.width);
    requirement.height = bitstream_candidate
        .requirement
        .height
        .or(requirement.height);
}

/// Записывает conflict, если container hint и bitstream value одновременно известны и отличаются.
fn record_requirement_conflict<T>(
    field: Vp9MetadataField,
    container_value: Option<T>,
    bitstream_value: Option<T>,
    diagnostics: &mut Vec<Vp9MetadataDiagnostic>,
) where
    T: PartialEq + ToString,
{
    let (Some(container_value), Some(bitstream_value)) = (container_value, bitstream_value) else {
        return;
    };

    if container_value == bitstream_value {
        return;
    }

    diagnostics.push(Vp9MetadataDiagnostic::FieldConflict(Vp9MetadataConflict {
        field,
        selected_origin: ColorMetadataOrigin::Bitstream,
        selected_value: bitstream_value.to_string(),
        rejected_origin: ColorMetadataOrigin::Container,
        rejected_value: container_value.to_string(),
    }));
}

/// Записывает conflict coded resolution, если оба источника дали разные размеры.
fn record_resolution_conflict(
    container_source: &Option<Vp9MetadataSource>,
    bitstream_candidate: &Vp9RequirementCandidate,
    diagnostics: &mut Vec<Vp9MetadataDiagnostic>,
) {
    let Some(container_source) = container_source else {
        return;
    };
    let container_resolution = container_source.width.zip(container_source.height);
    let bitstream_resolution = bitstream_candidate
        .requirement
        .width
        .zip(bitstream_candidate.requirement.height);
    let (Some(container_resolution), Some(bitstream_resolution)) =
        (container_resolution, bitstream_resolution)
    else {
        return;
    };

    if container_resolution == bitstream_resolution {
        return;
    }

    diagnostics.push(Vp9MetadataDiagnostic::FieldConflict(Vp9MetadataConflict {
        field: Vp9MetadataField::Resolution,
        selected_origin: ColorMetadataOrigin::Bitstream,
        selected_value: format!("{}x{}", bitstream_resolution.0, bitstream_resolution.1),
        rejected_origin: ColorMetadataOrigin::Container,
        rejected_value: format!("{}x{}", container_resolution.0, container_resolution.1),
    }));
}

/// Собирает итоговую color metadata: container Colour имеет приоритет, bitstream дополняет пропуски.
fn resolve_vp9_color_metadata(
    container_color: Option<&VideoColorMetadata>,
    bitstream_color: Option<&VideoColorMetadata>,
    diagnostics: &mut Vec<Vp9MetadataDiagnostic>,
) -> VideoColorMetadata {
    match (container_color, bitstream_color) {
        (Some(container_color), Some(bitstream_color)) => {
            record_color_conflicts(container_color, bitstream_color, diagnostics);
            merge_container_and_bitstream_color(container_color, bitstream_color)
        }
        (Some(container_color), None) => container_color.clone(),
        (None, Some(bitstream_color)) => bitstream_color.clone(),
        (None, None) => {
            diagnostics.push(Vp9MetadataDiagnostic::SdrFallbackApplied);
            VideoColorMetadata::sdr_bt709_limited()
        }
    }
}

/// Записывает field-level color conflicts без изменения выбранной policy.
fn record_color_conflicts(
    container_color: &VideoColorMetadata,
    bitstream_color: &VideoColorMetadata,
    diagnostics: &mut Vec<Vp9MetadataDiagnostic>,
) {
    record_color_conflict(
        Vp9MetadataField::Range,
        known_range_name(container_color.range),
        known_range_name(bitstream_color.range),
        diagnostics,
    );
    record_color_conflict(
        Vp9MetadataField::Matrix,
        known_matrix_name(container_color.matrix),
        known_matrix_name(bitstream_color.matrix),
        diagnostics,
    );
    record_color_conflict(
        Vp9MetadataField::Primaries,
        known_primaries_name(container_color.primaries),
        known_primaries_name(bitstream_color.primaries),
        diagnostics,
    );
    record_color_conflict(
        Vp9MetadataField::Transfer,
        known_transfer_name(container_color.transfer),
        known_transfer_name(bitstream_color.transfer),
        diagnostics,
    );
}

/// Записывает один color conflict, если оба значения известны и различаются.
fn record_color_conflict(
    field: Vp9MetadataField,
    container_value: Option<String>,
    bitstream_value: Option<String>,
    diagnostics: &mut Vec<Vp9MetadataDiagnostic>,
) {
    let (Some(container_value), Some(bitstream_value)) = (container_value, bitstream_value) else {
        return;
    };

    if container_value == bitstream_value {
        return;
    }

    diagnostics.push(Vp9MetadataDiagnostic::FieldConflict(Vp9MetadataConflict {
        field,
        selected_origin: ColorMetadataOrigin::Container,
        selected_value: container_value,
        rejected_origin: ColorMetadataOrigin::Bitstream,
        rejected_value: bitstream_value,
    }));
}

/// Заполняет неизвестные container color поля данными из bitstream header-а.
fn merge_container_and_bitstream_color(
    container_color: &VideoColorMetadata,
    bitstream_color: &VideoColorMetadata,
) -> VideoColorMetadata {
    let mut resolved_color = container_color.clone();
    if resolved_color.range == ColorRange::Unknown {
        resolved_color.range = bitstream_color.range;
    }
    if resolved_color.matrix == MatrixCoefficients::Unknown {
        resolved_color.matrix = bitstream_color.matrix;
    }
    if resolved_color.primaries == ColorPrimaries::Unknown {
        resolved_color.primaries = bitstream_color.primaries;
    }
    if resolved_color.transfer == TransferFunction::Unknown {
        resolved_color.transfer = bitstream_color.transfer;
    }
    resolved_color
}

/// Переводит VP9 bitstream color_space/color_range в общую color metadata.
fn bitstream_color_from_frame_info(
    frame_info: &vp9_parser::Vp9FrameInfo,
) -> Option<VideoColorMetadata> {
    let color_space = frame_info.color_space?;
    let (primaries, matrix, transfer) = match color_space {
        1 => (
            ColorPrimaries::Smpte170m,
            MatrixCoefficients::Bt601,
            TransferFunction::Bt709,
        ),
        2 => (
            ColorPrimaries::Bt709,
            MatrixCoefficients::Bt709,
            TransferFunction::Bt709,
        ),
        3 => (
            ColorPrimaries::Smpte170m,
            MatrixCoefficients::Bt601,
            TransferFunction::Bt709,
        ),
        5 => (
            ColorPrimaries::Bt2020,
            MatrixCoefficients::Bt2020,
            TransferFunction::Unknown,
        ),
        7 => (
            ColorPrimaries::Bt709,
            MatrixCoefficients::Unknown,
            TransferFunction::Srgb,
        ),
        _ => (
            ColorPrimaries::Unknown,
            MatrixCoefficients::Unknown,
            TransferFunction::Unknown,
        ),
    };
    let range = match frame_info.color_range_full {
        Some(true) => ColorRange::Full,
        Some(false) => ColorRange::Limited,
        None => ColorRange::Unknown,
    };

    Some(VideoColorMetadata::bitstream(
        range, matrix, primaries, transfer,
    ))
}

/// Проверяет обязательное значение strict HDR поля.
fn validate_required_value(
    field: Vp9MetadataField,
    actual: Option<String>,
    expected: Option<String>,
) -> Result<(), Vp9StrictHdrValidationError> {
    let Some(actual) = actual else {
        return Err(Vp9StrictHdrValidationError::MissingField(field));
    };

    let Some(expected) = expected else {
        return Ok(());
    };

    if actual == expected {
        Ok(())
    } else {
        Err(Vp9StrictHdrValidationError::InvalidField {
            field,
            expected,
            actual,
        })
    }
}

/// Возвращает stable name для known range или `None`, если поле неизвестно.
fn known_range_name(range: ColorRange) -> Option<String> {
    match range {
        ColorRange::Limited => Some("Limited".to_string()),
        ColorRange::Full => Some("Full".to_string()),
        ColorRange::Unknown => None,
    }
}

/// Возвращает stable name для known matrix или `None`, если поле неизвестно.
fn known_matrix_name(matrix: MatrixCoefficients) -> Option<String> {
    match matrix {
        MatrixCoefficients::Bt601 => Some("BT.601".to_string()),
        MatrixCoefficients::Bt709 => Some("BT.709".to_string()),
        MatrixCoefficients::Bt2020 => Some("BT.2020".to_string()),
        MatrixCoefficients::Unknown => None,
    }
}

/// Возвращает stable name для known primaries или `None`, если поле неизвестно.
fn known_primaries_name(primaries: ColorPrimaries) -> Option<String> {
    match primaries {
        ColorPrimaries::Bt709 => Some("BT.709".to_string()),
        ColorPrimaries::Bt2020 => Some("BT.2020".to_string()),
        ColorPrimaries::Smpte170m => Some("SMPTE 170M".to_string()),
        ColorPrimaries::Bt470Bg => Some("BT.470BG".to_string()),
        ColorPrimaries::Unknown => None,
    }
}

/// Возвращает stable name для known transfer или `None`, если поле неизвестно.
fn known_transfer_name(transfer: TransferFunction) -> Option<String> {
    match transfer {
        TransferFunction::Bt709 => Some("BT.709".to_string()),
        TransferFunction::Srgb => Some("sRGB".to_string()),
        TransferFunction::Pq => Some("PQ".to_string()),
        TransferFunction::Hlg => Some("HLG".to_string()),
        TransferFunction::Unknown => None,
    }
}

impl Vp9DecodedFormatRequirement {
    /// Возвращает stable decoded format label.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Nv12 => "NV12",
            Self::P010 => "P010",
        }
    }
}

/// Переводит сырое VP9 bit depth значение в typed model.
fn bit_depth_from_header(bit_depth: u8) -> Option<BitDepth> {
    match bit_depth {
        8 => Some(BitDepth::Eight),
        10 => Some(BitDepth::Ten),
        12 => Some(BitDepth::Twelve),
        _ => None,
    }
}

/// Выводит decoded format только для bit depths, допущенных Phase 9 adapter-ом.
fn decoded_format_from_supported_bit_depth(
    bit_depth: BitDepth,
) -> Option<Vp9DecodedFormatRequirement> {
    match bit_depth {
        BitDepth::Eight => Some(Vp9DecodedFormatRequirement::Nv12),
        BitDepth::Ten => Some(Vp9DecodedFormatRequirement::P010),
        BitDepth::Twelve => None,
    }
}

/// Переводит VP9 subsampling flags в общую chroma model.
fn chroma_from_subsampling_flags(
    subsampling_x: Option<bool>,
    subsampling_y: Option<bool>,
) -> Option<ChromaSubsampling> {
    match (subsampling_x, subsampling_y) {
        (Some(true), Some(true)) => Some(ChromaSubsampling::Yuv420),
        (Some(true), Some(false)) => Some(ChromaSubsampling::Yuv422),
        (Some(false), Some(false)) => Some(ChromaSubsampling::Yuv444),
        _ => None,
    }
}

/// Создаёт recoverable результат для неизвестной chroma layout.
fn recoverable_chroma_uncertainty(
    subsampling_x: Option<bool>,
    subsampling_y: Option<bool>,
) -> Vp9RequirementProbe {
    match (subsampling_x, subsampling_y) {
        (Some(subsampling_x), Some(subsampling_y)) => {
            Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::UnknownChromaLayout {
                subsampling_x,
                subsampling_y,
            })
        }
        _ => Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ParseError(
            "VP9 color config did not expose chroma subsampling".to_string(),
        )),
    }
}

/// Создаёт recoverable результат для неизвестного bit depth.
fn recoverable_bit_depth_uncertainty(bit_depth: u8) -> Vp9RequirementProbe {
    Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::UnknownBitDepth { bit_depth })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_MARKER: u32 = 0b10;
    const SYNC_CODE: u32 = 0x498342;

    #[test]
    fn profile0_8bit_yuv420_builds_nv12_requirement() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 0,
            bit_depth: 8,
            subsampling_x: true,
            subsampling_y: true,
            width: 64,
            height: 64,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        let Vp9RequirementProbe::Candidate(candidate) = probe else {
            panic!("profile0 8-bit 4:2:0 должен дать candidate, получено {probe:?}");
        };
        assert_eq!(candidate.decoded_format, Vp9DecodedFormatRequirement::Nv12);
        assert_eq!(
            candidate.requirement.profile,
            Some(VideoProfile::Vp9(Vp9Profile::Profile0))
        );
        assert_eq!(candidate.requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(
            candidate.requirement.chroma,
            Some(ChromaSubsampling::Yuv420)
        );
    }

    #[test]
    fn profile2_10bit_yuv420_builds_p010_candidate_requirement() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 10,
            subsampling_x: true,
            subsampling_y: true,
            width: 128,
            height: 72,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        let Vp9RequirementProbe::Candidate(candidate) = probe else {
            panic!("profile2 10-bit 4:2:0 должен дать P010 candidate, получено {probe:?}");
        };
        assert_eq!(candidate.decoded_format, Vp9DecodedFormatRequirement::P010);
        assert_eq!(
            candidate.requirement.profile,
            Some(VideoProfile::Vp9(Vp9Profile::Profile2))
        );
        assert_eq!(candidate.requirement.bit_depth, Some(BitDepth::Ten));
        assert_eq!(
            candidate.requirement.chroma,
            Some(ChromaSubsampling::Yuv420)
        );
    }

    #[test]
    fn container_bt2020_pq_profile2_10bit_is_strict_hdr_core_valid() {
        let container = container_hdr_source(TransferFunction::Pq);
        let bitstream_candidate = candidate_from_fixture(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 10,
            subsampling_x: true,
            subsampling_y: true,
            width: 3840,
            height: 2160,
        });

        let resolved_metadata = resolve_vp9_metadata(Some(container), Some(bitstream_candidate));

        assert_eq!(resolved_metadata.color.transfer, TransferFunction::Pq);
        assert_eq!(resolved_metadata.color.primaries, ColorPrimaries::Bt2020);
        assert_eq!(resolved_metadata.color.matrix, MatrixCoefficients::Bt2020);
        assert_eq!(resolved_metadata.color.range, ColorRange::Limited);
        assert_eq!(validate_vp9_strict_hdr_core(&resolved_metadata), Ok(()));
    }

    #[test]
    fn container_bt2020_hlg_profile2_10bit_is_strict_hdr_core_valid() {
        let container = container_hdr_source(TransferFunction::Hlg);
        let bitstream_candidate = candidate_from_fixture(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 10,
            subsampling_x: true,
            subsampling_y: true,
            width: 3840,
            height: 2160,
        });

        let resolved_metadata = resolve_vp9_metadata(Some(container), Some(bitstream_candidate));

        assert_eq!(resolved_metadata.color.transfer, TransferFunction::Hlg);
        assert_eq!(validate_vp9_strict_hdr_core(&resolved_metadata), Ok(()));
    }

    #[test]
    fn missing_transfer_for_hdr_candidate_is_typed_missing_metadata_rejection() {
        let container = container_hdr_source(TransferFunction::Unknown);
        let bitstream_candidate = profile2_10bit_candidate_without_color();

        let resolved_metadata = resolve_vp9_metadata(Some(container), Some(bitstream_candidate));

        assert_eq!(
            validate_vp9_strict_hdr_core(&resolved_metadata),
            Err(Vp9StrictHdrValidationError::MissingField(
                Vp9MetadataField::Transfer
            ))
        );
    }

    #[test]
    fn missing_strict_hdr_core_color_fields_are_rejected() {
        let cases = [
            (
                Vp9MetadataField::Transfer,
                hdr_color_with_core_field(
                    ColorRange::Limited,
                    MatrixCoefficients::Bt2020,
                    ColorPrimaries::Bt2020,
                    TransferFunction::Unknown,
                ),
            ),
            (
                Vp9MetadataField::Primaries,
                hdr_color_with_core_field(
                    ColorRange::Limited,
                    MatrixCoefficients::Bt2020,
                    ColorPrimaries::Unknown,
                    TransferFunction::Pq,
                ),
            ),
            (
                Vp9MetadataField::Matrix,
                hdr_color_with_core_field(
                    ColorRange::Limited,
                    MatrixCoefficients::Unknown,
                    ColorPrimaries::Bt2020,
                    TransferFunction::Pq,
                ),
            ),
            (
                Vp9MetadataField::Range,
                hdr_color_with_core_field(
                    ColorRange::Unknown,
                    MatrixCoefficients::Bt2020,
                    ColorPrimaries::Bt2020,
                    TransferFunction::Pq,
                ),
            ),
        ];

        for (missing_field, color) in cases {
            let container = container_hdr_source_with_color(color);
            let bitstream_candidate = profile2_10bit_candidate_without_color();
            let resolved_metadata =
                resolve_vp9_metadata(Some(container), Some(bitstream_candidate));

            assert_eq!(
                validate_vp9_strict_hdr_core(&resolved_metadata),
                Err(Vp9StrictHdrValidationError::MissingField(missing_field)),
                "expected {missing_field:?} to be rejected as missing"
            );
        }
    }

    #[test]
    fn bitstream_profile_conflict_with_container_hint_uses_bitstream_profile() {
        let container = Vp9MetadataSource::container()
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420);
        let bitstream_candidate = candidate_from_fixture(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 10,
            subsampling_x: true,
            subsampling_y: true,
            width: 1280,
            height: 720,
        });

        let resolved_metadata = resolve_vp9_metadata(Some(container), Some(bitstream_candidate));

        assert_eq!(
            resolved_metadata.requirement.profile,
            Some(VideoProfile::Vp9(Vp9Profile::Profile2))
        );
        assert!(resolved_metadata.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic,
                Vp9MetadataDiagnostic::FieldConflict(conflict)
                    if conflict.field == Vp9MetadataField::Profile
                        && conflict.selected_origin == ColorMetadataOrigin::Bitstream
            )
        }));
    }

    #[test]
    fn container_bitstream_color_conflict_records_diagnostic() {
        let container = container_hdr_source(TransferFunction::Pq);
        let bitstream_candidate = candidate_from_fixture(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 10,
            subsampling_x: true,
            subsampling_y: true,
            width: 3840,
            height: 2160,
        });

        let resolved_metadata = resolve_vp9_metadata(Some(container), Some(bitstream_candidate));

        assert!(resolved_metadata.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic,
                Vp9MetadataDiagnostic::FieldConflict(conflict)
                    if conflict.field == Vp9MetadataField::Matrix
                        && conflict.selected_origin == ColorMetadataOrigin::Container
                        && conflict.rejected_origin == ColorMetadataOrigin::Bitstream
            )
        }));
    }

    #[test]
    fn profile2_12bit_is_explicit_unsupported_bit_depth() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 12,
            subsampling_x: true,
            subsampling_y: true,
            width: 128,
            height: 72,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        assert_eq!(
            probe,
            Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedBitDepth(12))
        );
    }

    #[test]
    fn profile1_yuv422_is_explicit_unsupported_chroma() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 1,
            bit_depth: 8,
            subsampling_x: true,
            subsampling_y: false,
            width: 128,
            height: 72,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        assert_eq!(
            probe,
            Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedChroma(
                ChromaSubsampling::Yuv422
            ))
        );
    }

    #[test]
    fn profile3_yuv444_is_explicit_unsupported_chroma() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 3,
            bit_depth: 10,
            subsampling_x: false,
            subsampling_y: false,
            width: 128,
            height: 72,
        });

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        assert_eq!(
            probe,
            Vp9RequirementProbe::Rejected(Vp9RequirementRejection::UnsupportedChroma(
                ChromaSubsampling::Yuv444
            ))
        );
    }

    #[test]
    fn incomplete_packet_is_recoverable() {
        let probe = probe_vp9_packet_requirement(&[0x00]);

        assert!(matches!(
            probe,
            Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::ParseError(_))
        ));
    }

    #[test]
    fn inter_frame_without_color_config_is_recoverable() {
        let packet_bytes = build_inter_frame_without_refs();

        let probe = probe_vp9_packet_requirement(&packet_bytes);

        assert_eq!(
            probe,
            Vp9RequirementProbe::Recoverable(Vp9RequirementUncertainty::MissingColorConfig {
                profile: Vp9Profile::Profile0
            })
        );
    }

    struct Vp9HeaderFixture {
        profile: u8,
        bit_depth: u8,
        subsampling_x: bool,
        subsampling_y: bool,
        width: u32,
        height: u32,
    }

    fn container_hdr_source(transfer: TransferFunction) -> Vp9MetadataSource {
        container_hdr_source_with_color(VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            transfer,
            None,
        ))
    }

    fn container_hdr_source_with_color(color: VideoColorMetadata) -> Vp9MetadataSource {
        Vp9MetadataSource::container()
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420)
            .with_color(color)
    }

    fn hdr_color_with_core_field(
        range: ColorRange,
        matrix: MatrixCoefficients,
        primaries: ColorPrimaries,
        transfer: TransferFunction,
    ) -> VideoColorMetadata {
        VideoColorMetadata::container(range, matrix, primaries, transfer, None)
    }

    fn candidate_from_fixture(fixture: Vp9HeaderFixture) -> Vp9RequirementCandidate {
        let packet_bytes = build_vp9_keyframe(fixture);
        let Vp9RequirementProbe::Candidate(candidate) = probe_vp9_packet_requirement(&packet_bytes)
        else {
            panic!("fixture должен давать VP9 candidate");
        };
        candidate
    }

    fn profile2_10bit_candidate_without_color() -> Vp9RequirementCandidate {
        Vp9RequirementCandidate {
            requirement: VideoDecodeRequirement::new(VideoCodec::Vp9)
                .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
                .with_bit_depth(BitDepth::Ten)
                .with_chroma(ChromaSubsampling::Yuv420)
                .with_resolution(3840, 2160),
            decoded_format: Vp9DecodedFormatRequirement::P010,
            bitstream_color: None,
        }
    }

    fn build_vp9_keyframe(fixture: Vp9HeaderFixture) -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, FRAME_MARKER, 2);
        push_profile(&mut bits, fixture.profile);
        bits.push(0);
        bits.push(0);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, SYNC_CODE, 24);
        if matches!(fixture.profile, 2 | 3) {
            bits.push(u8::from(fixture.bit_depth == 12));
        }
        push_bits(&mut bits, 1, 3);
        bits.push(0);
        if matches!(fixture.profile, 1 | 3) {
            bits.push(u8::from(fixture.subsampling_x));
            bits.push(u8::from(fixture.subsampling_y));
            bits.push(0);
        }
        push_bits(&mut bits, fixture.width - 1, 16);
        push_bits(&mut bits, fixture.height - 1, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn build_inter_frame_without_refs() -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, FRAME_MARKER, 2);
        push_profile(&mut bits, 0);
        bits.push(0);
        bits.push(1);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0, 2);
        push_bits(&mut bits, 0x01, 8);
        push_bits(&mut bits, 1, 3);
        bits.push(1);
        push_bits(&mut bits, 2, 3);
        bits.push(0);
        push_bits(&mut bits, 3, 3);
        bits.push(1);
        bits.push(0);
        bits.push(0);
        bits.push(0);
        push_bits(&mut bits, 63, 16);
        push_bits(&mut bits, 63, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                let mut byte = 0u8;
                for (index, bit) in chunk.iter().enumerate() {
                    byte |= bit << (7 - index);
                }
                byte
            })
            .collect()
    }

    fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) as u8);
        }
    }

    fn push_profile(bits: &mut Vec<u8>, profile: u8) {
        bits.push(profile & 1);
        bits.push((profile >> 1) & 1);
        if profile == 3 {
            bits.push(0);
        }
    }
}
