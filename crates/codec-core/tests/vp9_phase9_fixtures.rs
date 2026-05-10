use std::collections::BTreeMap;

use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction,
    VideoColorMetadata, VideoProfile, Vp9DecodedFormatRequirement, Vp9MetadataDiagnostic,
    Vp9MetadataField, Vp9MetadataSource, Vp9Profile, Vp9RequirementCandidate, Vp9RequirementProbe,
    Vp9RequirementRejection, probe_vp9_packet_requirement, resolve_vp9_metadata,
    validate_vp9_strict_hdr_core,
};
use serde::Deserialize;

/// Raw golden VP9 headers, которые должны оставаться маленькими и стабильными.
const VP9_GOLDEN_HEADERS_JSON: &str = include_str!("fixtures/vp9_golden_headers.json");

/// Metadata resolver cases, которые покрывают HDR core и conflict diagnostics.
const VP9_METADATA_CASES_JSON: &str = include_str!("fixtures/vp9_metadata_cases.json");

#[derive(Debug, Deserialize)]
struct GoldenHeaderFixture {
    /// Человекочитаемый id, чтобы metadata fixtures могли ссылаться на raw header.
    name: String,

    /// Минимальный VP9 uncompressed header packet в hex-форме.
    packet_hex: String,

    /// Ожидаемый результат Phase 9 requirement probe.
    expected: ExpectedHeaderProbe,
}

#[derive(Debug, Deserialize)]
struct ExpectedHeaderProbe {
    /// `candidate` означает playable/readiness candidate, `rejected` - typed policy reject.
    kind: ExpectedProbeKind,

    /// Ожидаемый VP9 profile для candidate header-а.
    profile: Option<FixtureVp9Profile>,

    /// Ожидаемая bit depth для candidate header-а.
    bit_depth: Option<FixtureBitDepth>,

    /// Ожидаемая chroma layout для candidate header-а.
    chroma: Option<FixtureChroma>,

    /// Ожидаемый decoded boundary format для candidate header-а.
    decoded_format: Option<FixtureDecodedFormat>,

    /// Ожидаемая coded width.
    width: Option<u32>,

    /// Ожидаемая coded height.
    height: Option<u32>,

    /// Ожидаемый color range из bitstream color config.
    range: Option<FixtureColorRange>,

    /// Ожидаемая matrix из bitstream color config.
    matrix: Option<FixtureMatrix>,

    /// Ожидаемые primaries из bitstream color config.
    primaries: Option<FixturePrimaries>,

    /// Ожидаемая transfer function из bitstream color config.
    transfer: Option<FixtureTransfer>,

    /// Тип expected reject-а для unsupported VP9 variants.
    rejection: Option<ExpectedRejectionKind>,

    /// Значение unsupported bit depth для 12-bit reject-а.
    unsupported_bit_depth: Option<u8>,

    /// Значение unsupported chroma для Profile 1/3 reject-а.
    unsupported_chroma: Option<FixtureChroma>,
}

#[derive(Debug, Deserialize)]
struct MetadataFixture {
    /// Человекочитаемый id fixture-а.
    name: String,

    /// Container-level metadata hints.
    container: MetadataSourceFixture,

    /// Имя raw VP9 header fixture-а, который даёт confirmed bitstream fields.
    bitstream_header: String,

    /// Ожидаемый результат field-level resolver-а.
    expected: ExpectedMetadataResolution,
}

#[derive(Debug, Deserialize)]
struct MetadataSourceFixture {
    /// Container VP9 profile hint.
    profile: FixtureVp9Profile,

    /// Container bit depth hint.
    bit_depth: FixtureBitDepth,

    /// Container chroma hint.
    chroma: FixtureChroma,

    /// Container coded width hint.
    width: u32,

    /// Container coded height hint.
    height: u32,

    /// Container color metadata hint.
    color: ColorFixture,
}

#[derive(Debug, Deserialize)]
struct ColorFixture {
    /// Container color range.
    range: FixtureColorRange,

    /// Container matrix coefficients.
    matrix: FixtureMatrix,

    /// Container color primaries.
    primaries: FixturePrimaries,

    /// Container transfer function.
    transfer: FixtureTransfer,
}

#[derive(Debug, Deserialize)]
struct ExpectedMetadataResolution {
    /// Итоговый profile после precedence.
    profile: FixtureVp9Profile,

    /// Итоговая bit depth после precedence.
    bit_depth: FixtureBitDepth,

    /// Итоговая chroma после precedence.
    chroma: FixtureChroma,

    /// Итоговый decoded format.
    decoded_format: FixtureDecodedFormat,

    /// Итоговая coded width.
    width: u32,

    /// Итоговая coded height.
    height: u32,

    /// Итоговый color range.
    range: FixtureColorRange,

    /// Итоговая matrix.
    matrix: FixtureMatrix,

    /// Итоговые primaries.
    primaries: FixturePrimaries,

    /// Итоговая transfer function.
    transfer: FixtureTransfer,

    /// Должен ли strict HDR core пройти.
    strict_hdr_core_valid: bool,

    /// Ожидаемые conflict fields в порядке записи diagnostics.
    conflict_fields: Vec<FixtureMetadataField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExpectedProbeKind {
    /// Header должен стать candidate requirement.
    Candidate,

    /// Header должен стать typed reject-ом.
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExpectedRejectionKind {
    /// VP9 12-bit rejected.
    UnsupportedBitDepth,

    /// VP9 4:2:2/4:4:4 rejected.
    UnsupportedChroma,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureVp9Profile {
    /// VP9 Profile 0.
    Profile0,

    /// VP9 Profile 1.
    Profile1,

    /// VP9 Profile 2.
    Profile2,

    /// VP9 Profile 3.
    Profile3,
}

impl FixtureVp9Profile {
    /// Переводит fixture enum в production model.
    const fn into_model(self) -> Vp9Profile {
        match self {
            Self::Profile0 => Vp9Profile::Profile0,
            Self::Profile1 => Vp9Profile::Profile1,
            Self::Profile2 => Vp9Profile::Profile2,
            Self::Profile3 => Vp9Profile::Profile3,
        }
    }

    /// Переводит fixture enum в codec-specific profile.
    const fn into_video_profile(self) -> VideoProfile {
        VideoProfile::Vp9(self.into_model())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureBitDepth {
    /// 8-bit.
    Eight,

    /// 10-bit.
    Ten,

    /// 12-bit.
    Twelve,
}

impl FixtureBitDepth {
    /// Переводит fixture enum в production model.
    const fn into_model(self) -> BitDepth {
        match self {
            Self::Eight => BitDepth::Eight,
            Self::Ten => BitDepth::Ten,
            Self::Twelve => BitDepth::Twelve,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureChroma {
    /// 4:2:0.
    Yuv420,

    /// 4:2:2.
    Yuv422,

    /// 4:4:4.
    Yuv444,
}

impl FixtureChroma {
    /// Переводит fixture enum в production model.
    const fn into_model(self) -> ChromaSubsampling {
        match self {
            Self::Yuv420 => ChromaSubsampling::Yuv420,
            Self::Yuv422 => ChromaSubsampling::Yuv422,
            Self::Yuv444 => ChromaSubsampling::Yuv444,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureDecodedFormat {
    /// NV12 decoded boundary.
    Nv12,

    /// P010 decoded boundary.
    P010,
}

impl FixtureDecodedFormat {
    /// Переводит fixture enum в VP9 decoded format model.
    const fn into_model(self) -> Vp9DecodedFormatRequirement {
        match self {
            Self::Nv12 => Vp9DecodedFormatRequirement::Nv12,
            Self::P010 => Vp9DecodedFormatRequirement::P010,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureColorRange {
    /// Limited/video range.
    Limited,

    /// Full range.
    Full,

    /// Unknown range.
    Unknown,
}

impl FixtureColorRange {
    /// Переводит fixture enum в production model.
    const fn into_model(self) -> ColorRange {
        match self {
            Self::Limited => ColorRange::Limited,
            Self::Full => ColorRange::Full,
            Self::Unknown => ColorRange::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureMatrix {
    /// BT.601 matrix.
    Bt601,

    /// BT.709 matrix.
    Bt709,

    /// BT.2020 matrix.
    Bt2020,

    /// Unknown matrix.
    Unknown,
}

impl FixtureMatrix {
    /// Переводит fixture enum в production model.
    const fn into_model(self) -> MatrixCoefficients {
        match self {
            Self::Bt601 => MatrixCoefficients::Bt601,
            Self::Bt709 => MatrixCoefficients::Bt709,
            Self::Bt2020 => MatrixCoefficients::Bt2020,
            Self::Unknown => MatrixCoefficients::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixturePrimaries {
    /// BT.709 primaries.
    Bt709,

    /// BT.2020 primaries.
    Bt2020,

    /// SMPTE 170M primaries.
    Smpte170m,

    /// Unknown primaries.
    Unknown,
}

impl FixturePrimaries {
    /// Переводит fixture enum в production model.
    const fn into_model(self) -> ColorPrimaries {
        match self {
            Self::Bt709 => ColorPrimaries::Bt709,
            Self::Bt2020 => ColorPrimaries::Bt2020,
            Self::Smpte170m => ColorPrimaries::Smpte170m,
            Self::Unknown => ColorPrimaries::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureTransfer {
    /// BT.709 transfer.
    Bt709,

    /// PQ transfer.
    Pq,

    /// HLG transfer.
    Hlg,

    /// Unknown transfer.
    Unknown,
}

impl FixtureTransfer {
    /// Переводит fixture enum в production model.
    const fn into_model(self) -> TransferFunction {
        match self {
            Self::Bt709 => TransferFunction::Bt709,
            Self::Pq => TransferFunction::Pq,
            Self::Hlg => TransferFunction::Hlg,
            Self::Unknown => TransferFunction::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureMetadataField {
    /// Profile conflict.
    Profile,

    /// Bit depth conflict.
    BitDepth,

    /// Resolution conflict.
    Resolution,

    /// Matrix conflict.
    Matrix,

    /// Primaries conflict.
    Primaries,
}

impl FixtureMetadataField {
    /// Переводит fixture enum в production diagnostic field.
    const fn into_model(self) -> Vp9MetadataField {
        match self {
            Self::Profile => Vp9MetadataField::Profile,
            Self::BitDepth => Vp9MetadataField::BitDepth,
            Self::Resolution => Vp9MetadataField::Resolution,
            Self::Matrix => Vp9MetadataField::Matrix,
            Self::Primaries => Vp9MetadataField::Primaries,
        }
    }
}

#[test]
fn all_golden_headers_parse_to_expected_requirements() {
    let fixtures = golden_header_fixtures();

    for fixture in fixtures {
        let packet_bytes = decode_hex_payload(&fixture.packet_hex);
        let probe = probe_vp9_packet_requirement(&packet_bytes);

        match fixture.expected.kind {
            ExpectedProbeKind::Candidate => {
                let Vp9RequirementProbe::Candidate(candidate) = probe else {
                    panic!(
                        "{} должен быть VP9 candidate, получено {probe:?}",
                        fixture.name
                    );
                };
                assert_candidate_matches_fixture(&fixture.name, &candidate, &fixture.expected);
            }
            ExpectedProbeKind::Rejected => {
                let Vp9RequirementProbe::Rejected(rejection) = probe else {
                    panic!(
                        "{} должен быть typed VP9 reject, получено {probe:?}",
                        fixture.name
                    );
                };
                assert_rejection_matches_fixture(&fixture.name, rejection, &fixture.expected);
            }
        }
    }
}

#[test]
fn metadata_fixtures_resolve_to_expected_fields() {
    let headers_by_name = golden_headers_by_name();
    let metadata_fixtures = metadata_fixtures();

    for fixture in metadata_fixtures {
        let header = headers_by_name
            .get(&fixture.bitstream_header)
            .unwrap_or_else(|| {
                panic!(
                    "metadata fixture {} ссылается на неизвестный header",
                    fixture.name
                )
            });
        let bitstream_candidate = candidate_from_header_fixture(header);
        let container_source = metadata_source_from_fixture(&fixture.container);
        let resolved_metadata =
            resolve_vp9_metadata(Some(container_source), Some(bitstream_candidate));

        assert_eq!(
            resolved_metadata.requirement.profile,
            Some(fixture.expected.profile.into_video_profile()),
            "{}: unexpected profile",
            fixture.name
        );
        assert_eq!(
            resolved_metadata.requirement.bit_depth,
            Some(fixture.expected.bit_depth.into_model()),
            "{}: unexpected bit depth",
            fixture.name
        );
        assert_eq!(
            resolved_metadata.requirement.chroma,
            Some(fixture.expected.chroma.into_model()),
            "{}: unexpected chroma",
            fixture.name
        );
        assert_eq!(
            resolved_metadata.decoded_format,
            Some(fixture.expected.decoded_format.into_model()),
            "{}: unexpected decoded format",
            fixture.name
        );
        assert_eq!(
            resolved_metadata.requirement.width,
            Some(fixture.expected.width),
            "{}: unexpected width",
            fixture.name
        );
        assert_eq!(
            resolved_metadata.requirement.height,
            Some(fixture.expected.height),
            "{}: unexpected height",
            fixture.name
        );
        assert_color_matches_expected(
            &fixture.name,
            &resolved_metadata.color,
            fixture.expected.range,
            fixture.expected.matrix,
            fixture.expected.primaries,
            fixture.expected.transfer,
        );
        assert_eq!(
            validate_vp9_strict_hdr_core(&resolved_metadata).is_ok(),
            fixture.expected.strict_hdr_core_valid,
            "{}: unexpected strict HDR core result",
            fixture.name
        );
    }
}

#[test]
fn conflict_fixture_records_expected_conflicts() {
    let headers_by_name = golden_headers_by_name();
    let fixture = metadata_fixtures()
        .into_iter()
        .find(|fixture| fixture.name == "container_profile0_conflicts_with_profile2_bitstream")
        .expect("conflict fixture должен быть описан в JSON");
    let header = headers_by_name
        .get(&fixture.bitstream_header)
        .expect("conflict fixture должен ссылаться на golden header");
    let resolved_metadata = resolve_vp9_metadata(
        Some(metadata_source_from_fixture(&fixture.container)),
        Some(candidate_from_header_fixture(header)),
    );
    let actual_conflict_fields = conflict_fields_from_diagnostics(&resolved_metadata.diagnostics);
    let expected_conflict_fields = fixture
        .expected
        .conflict_fields
        .into_iter()
        .map(FixtureMetadataField::into_model)
        .collect::<Vec<_>>();

    assert_eq!(actual_conflict_fields, expected_conflict_fields);
}

/// Читает raw golden header fixtures из JSON.
fn golden_header_fixtures() -> Vec<GoldenHeaderFixture> {
    serde_json::from_str(VP9_GOLDEN_HEADERS_JSON)
        .expect("VP9 golden header fixtures должны быть валидным JSON")
}

/// Читает metadata resolver fixtures из JSON.
fn metadata_fixtures() -> Vec<MetadataFixture> {
    serde_json::from_str(VP9_METADATA_CASES_JSON)
        .expect("VP9 metadata fixtures должны быть валидным JSON")
}

/// Индексирует golden headers по имени для metadata fixtures.
fn golden_headers_by_name() -> BTreeMap<String, GoldenHeaderFixture> {
    golden_header_fixtures()
        .into_iter()
        .map(|fixture| (fixture.name.clone(), fixture))
        .collect()
}

/// Декодирует compact hex payload без дополнительной runtime dependency.
fn decode_hex_payload(packet_hex: &str) -> Vec<u8> {
    assert!(
        packet_hex.len() % 2 == 0,
        "VP9 fixture hex payload должен иметь чётную длину"
    );

    packet_hex
        .as_bytes()
        .chunks_exact(2)
        .map(|byte_digits| {
            let byte_text = std::str::from_utf8(byte_digits)
                .expect("VP9 fixture hex payload должен быть ASCII/UTF-8");
            u8::from_str_radix(byte_text, 16).expect("VP9 fixture hex payload должен быть hex")
        })
        .collect()
}

/// Проверяет candidate requirement и bitstream color metadata.
fn assert_candidate_matches_fixture(
    fixture_name: &str,
    candidate: &Vp9RequirementCandidate,
    expected: &ExpectedHeaderProbe,
) {
    assert_eq!(
        candidate.requirement.profile,
        expected.profile.map(FixtureVp9Profile::into_video_profile),
        "{fixture_name}: unexpected profile"
    );
    assert_eq!(
        candidate.requirement.bit_depth,
        expected.bit_depth.map(FixtureBitDepth::into_model),
        "{fixture_name}: unexpected bit depth"
    );
    assert_eq!(
        candidate.requirement.chroma,
        expected.chroma.map(FixtureChroma::into_model),
        "{fixture_name}: unexpected chroma"
    );
    assert_eq!(
        candidate.decoded_format,
        expected
            .decoded_format
            .expect("candidate fixture должен указать decoded_format")
            .into_model(),
        "{fixture_name}: unexpected decoded format"
    );
    assert_eq!(
        candidate.requirement.width, expected.width,
        "{fixture_name}: unexpected width"
    );
    assert_eq!(
        candidate.requirement.height, expected.height,
        "{fixture_name}: unexpected height"
    );

    let bitstream_color = candidate
        .bitstream_color
        .as_ref()
        .expect("candidate fixture должен иметь bitstream color metadata");
    assert_color_matches_expected(
        fixture_name,
        bitstream_color,
        expected
            .range
            .expect("candidate fixture должен указать range"),
        expected
            .matrix
            .expect("candidate fixture должен указать matrix"),
        expected
            .primaries
            .expect("candidate fixture должен указать primaries"),
        expected
            .transfer
            .expect("candidate fixture должен указать transfer"),
    );
}

/// Проверяет typed VP9 reject из golden header-а.
fn assert_rejection_matches_fixture(
    fixture_name: &str,
    rejection: Vp9RequirementRejection,
    expected: &ExpectedHeaderProbe,
) {
    match expected
        .rejection
        .expect("rejected fixture должен указать rejection kind")
    {
        ExpectedRejectionKind::UnsupportedBitDepth => {
            assert_eq!(
                rejection,
                Vp9RequirementRejection::UnsupportedBitDepth(
                    expected
                        .unsupported_bit_depth
                        .expect("unsupported_bit_depth должен быть указан")
                ),
                "{fixture_name}: unexpected bit-depth rejection"
            );
        }
        ExpectedRejectionKind::UnsupportedChroma => {
            assert_eq!(
                rejection,
                Vp9RequirementRejection::UnsupportedChroma(
                    expected
                        .unsupported_chroma
                        .expect("unsupported_chroma должен быть указан")
                        .into_model()
                ),
                "{fixture_name}: unexpected chroma rejection"
            );
        }
    }
}

/// Получает candidate из raw header fixture и падает, если fixture описан как reject.
fn candidate_from_header_fixture(fixture: &GoldenHeaderFixture) -> Vp9RequirementCandidate {
    let packet_bytes = decode_hex_payload(&fixture.packet_hex);
    let probe = probe_vp9_packet_requirement(&packet_bytes);
    let Vp9RequirementProbe::Candidate(candidate) = probe else {
        panic!(
            "{} должен давать bitstream candidate для metadata fixture, получено {probe:?}",
            fixture.name
        );
    };
    candidate
}

/// Создаёт typed VP9 metadata source из fixture JSON.
fn metadata_source_from_fixture(fixture: &MetadataSourceFixture) -> Vp9MetadataSource {
    Vp9MetadataSource::container()
        .with_profile(fixture.profile.into_video_profile())
        .with_bit_depth(fixture.bit_depth.into_model())
        .with_chroma(fixture.chroma.into_model())
        .with_resolution(fixture.width, fixture.height)
        .with_color(VideoColorMetadata::container(
            fixture.color.range.into_model(),
            fixture.color.matrix.into_model(),
            fixture.color.primaries.into_model(),
            fixture.color.transfer.into_model(),
            None,
        ))
}

/// Проверяет четыре основные colorimetry поля.
fn assert_color_matches_expected(
    fixture_name: &str,
    color: &VideoColorMetadata,
    range: FixtureColorRange,
    matrix: FixtureMatrix,
    primaries: FixturePrimaries,
    transfer: FixtureTransfer,
) {
    assert_eq!(
        color.range,
        range.into_model(),
        "{fixture_name}: unexpected color range"
    );
    assert_eq!(
        color.matrix,
        matrix.into_model(),
        "{fixture_name}: unexpected matrix"
    );
    assert_eq!(
        color.primaries,
        primaries.into_model(),
        "{fixture_name}: unexpected primaries"
    );
    assert_eq!(
        color.transfer,
        transfer.into_model(),
        "{fixture_name}: unexpected transfer"
    );
}

/// Достаёт field list из resolver diagnostics без строкового парсинга.
fn conflict_fields_from_diagnostics(
    diagnostics: &[Vp9MetadataDiagnostic],
) -> Vec<Vp9MetadataField> {
    diagnostics
        .iter()
        .filter_map(|diagnostic| match diagnostic {
            Vp9MetadataDiagnostic::FieldConflict(conflict) => Some(conflict.field),
            Vp9MetadataDiagnostic::SdrFallbackApplied => None,
        })
        .collect()
}
