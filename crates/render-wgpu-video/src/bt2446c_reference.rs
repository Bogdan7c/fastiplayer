use anyhow::{Result, bail, ensure};
use codec_core::{
    ColorMetadataConfidence, ColorMetadataOrigin, ColorPrimaries, ColorRange, HdrMetadata,
    MatrixCoefficients, TransferFunction, VideoColorMetadata,
};
use render_core::{HdrOutputMode, HdrToSdrSettings, HdrToneMappingOperator};

/// Минимальное значение 10-bit P010 code value.
const P010_10BIT_MIN_CODE_VALUE: f64 = 0.0;

/// Максимальное значение 10-bit P010 code value.
const P010_10BIT_MAX_CODE_VALUE: f64 = 1023.0;

/// Limited-range black для 10-bit luma.
const P010_LIMITED_LUMA_BLACK_CODE_VALUE: f64 = 64.0;

/// Limited-range white для 10-bit luma.
const P010_LIMITED_LUMA_WHITE_CODE_VALUE: f64 = 940.0;

/// Нейтральное значение 10-bit chroma.
const P010_CHROMA_CENTER_CODE_VALUE: f64 = 512.0;

/// Limited-range minimum для 10-bit chroma.
const P010_LIMITED_CHROMA_MIN_CODE_VALUE: f64 = 64.0;

/// Limited-range maximum для 10-bit chroma.
const P010_LIMITED_CHROMA_MAX_CODE_VALUE: f64 = 960.0;

/// BT.2020 non-constant-luminance Kr.
const BT2020_KR: f64 = 0.2627;

/// BT.2020 non-constant-luminance Kb.
const BT2020_KB: f64 = 0.0593;

/// BT.2020 non-constant-luminance Kg.
const BT2020_KG: f64 = 1.0 - BT2020_KR - BT2020_KB;

/// ST 2084 / PQ constant m1.
const PQ_M1: f64 = 2610.0 / 16_384.0;

/// ST 2084 / PQ constant m2.
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;

/// ST 2084 / PQ constant c1.
const PQ_C1: f64 = 3424.0 / 4096.0;

/// ST 2084 / PQ constant c2.
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;

/// ST 2084 / PQ constant c3.
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

/// Абсолютный peak PQ reference display в nits.
const PQ_REFERENCE_PEAK_NITS: f64 = 10_000.0;

/// BT.2100 HLG OETF constant a.
const HLG_A: f64 = 0.178_832_77;

/// BT.2100 HLG OETF constant b.
const HLG_B: f64 = 0.284_668_92;

/// BT.2100 HLG OETF constant c.
const HLG_C: f64 = 0.559_910_729_529_562;

/// BT.2100 HLG reference system gamma for 1 000 nit display.
const HLG_REFERENCE_SYSTEM_GAMMA: f64 = 1.2;

/// BT.2446-C crosstalk alpha used by this deterministic reference.
const BT2446C_REFERENCE_CROSSTALK_ALPHA: f64 = 0.04;

/// BT.2446-C luminance curve k1.
const BT2446C_K1: f64 = 0.838_02;

/// BT.2446-C luminance curve k2.
const BT2446C_K2: f64 = 15.099_68;

/// BT.2446-C luminance curve k3.
const BT2446C_K3: f64 = 0.742_04;

/// BT.2446-C luminance curve k4.
const BT2446C_K4: f64 = 78.994_39;

/// BT.2446-C SDR luminance knee point in nits.
const BT2446C_SDR_INFLECTION_NITS: f64 = 58.5;

/// BT.1886 reference gamma with zero black level.
const BT1886_REFERENCE_GAMMA: f64 = 2.4;

/// D65 x chromaticity used only for zero-luminance black fallback.
const D65_WHITE_X: f64 = 0.3127;

/// D65 y chromaticity used only for zero-luminance black fallback.
const D65_WHITE_Y: f64 = 0.3290;

/// BT.2020 linear RGB to CIE XYZ matrix from BT.2446-C.
const BT2020_LINEAR_RGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.6370, 0.1446, 0.1689],
    [0.2627, 0.6780, 0.0593],
    [0.0000, 0.0281, 1.0610],
];

/// CIE XYZ to BT.709 linear RGB matrix from BT.2446-C.
const XYZ_TO_BT709_LINEAR_RGB: [[f64; 3]; 3] = [
    [1.7167, -0.3557, -0.2534],
    [-0.6667, 1.6165, 0.0158],
    [0.0176, -0.0428, 0.9421],
];

/// 10-bit P010 sample in code values, before range normalization.
#[derive(Debug, Clone, Copy, PartialEq)]
struct P010CodeValues {
    /// Luma code value.
    y: f64,

    /// Cb code value.
    cb: f64,

    /// Cr code value.
    cr: f64,
}

/// Normalized YCbCr sample: luma in video signal units, chroma around zero.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NormalizedYuv {
    /// Normalized luma.
    y: f64,

    /// Normalized Cb.
    cb: f64,

    /// Normalized Cr.
    cr: f64,
}

/// RGB triple with explicit channel names to avoid positional ambiguity.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rgb {
    /// Red channel.
    red: f64,

    /// Green channel.
    green: f64,

    /// Blue channel.
    blue: f64,
}

impl Rgb {
    /// Создаёт RGB triple без скрытой нормализации или clamp.
    const fn new(red: f64, green: f64, blue: f64) -> Self {
        Self { red, green, blue }
    }

    /// Возвращает каналы в стабильном порядке для test assertions.
    const fn channels(self) -> [f64; 3] {
        [self.red, self.green, self.blue]
    }

    /// Проверяет finite state всех каналов.
    fn is_finite(self) -> bool {
        self.red.is_finite() && self.green.is_finite() && self.blue.is_finite()
    }
}

/// CIE XYZ triple.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Xyz {
    /// X tristimulus value.
    x: f64,

    /// Y luminance value.
    y: f64,

    /// Z tristimulus value.
    z: f64,
}

impl Xyz {
    /// Проверяет finite state всех XYZ компонентов.
    fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

/// CIE xy chromaticity pair.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Chromaticity {
    /// x chromaticity coordinate.
    x: f64,

    /// y chromaticity coordinate.
    y: f64,
}

impl Chromaticity {
    /// Проверяет finite state chromaticity pair.
    fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// Test-only settings for the CPU reference.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bt2446CReferenceSettings {
    /// Public Phase 10 HDR-to-SDR settings.
    hdr_to_sdr: HdrToSdrSettings,

    /// BT.2446-C crosstalk alpha parameter.
    crosstalk_alpha: f64,
}

impl Bt2446CReferenceSettings {
    /// Создаёт deterministic defaults for Phase 10 Session 4 reference vectors.
    const fn default_reference() -> Self {
        Self {
            hdr_to_sdr: HdrToSdrSettings::bt2446_c_sdr_bt709(),
            crosstalk_alpha: BT2446C_REFERENCE_CROSSTALK_ALPHA,
        }
    }
}

/// Input contract for the isolated CPU reference conversion.
#[derive(Debug, Clone, PartialEq)]
struct Bt2446CReferenceInput {
    /// P010 code values before range normalization.
    sample: P010CodeValues,

    /// Strict HDR color metadata from decoder/render boundary.
    color: VideoColorMetadata,

    /// Reference settings used for deterministic tests.
    settings: Bt2446CReferenceSettings,
}

/// Full CPU-reference trace, intentionally exposing intermediate values.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bt2446CReferenceOutput {
    /// YCbCr sample after 10-bit range normalization.
    normalized_yuv: NormalizedYuv,

    /// BT.2020 non-linear RGB before transfer decode.
    nonlinear_bt2020_rgb: Rgb,

    /// HDR linear/display-light RGB in nits.
    linear_hdr_rgb: Rgb,

    /// HDR RGB after BT.2446-C crosstalk matrix.
    crosstalk_hdr_rgb: Rgb,

    /// HDR XYZ after BT.2020 RGB to XYZ.
    hdr_xyz: Xyz,

    /// HDR xy chromaticity preserved through luminance tone mapping.
    hdr_chromaticity: Chromaticity,

    /// BT.2446-C tone-mapped SDR luminance in nits.
    tone_mapped_sdr_luminance_nits: f64,

    /// BT.709 linear RGB before inverse crosstalk.
    crosstalk_sdr_rgb: Rgb,

    /// BT.709 linear RGB after inverse crosstalk and before final clamp/OETF.
    linear_sdr_rgb_before_final_clamp: Rgb,

    /// SDR BT.709/BT.1886 encoded RGB after the only allowed final clamp.
    encoded_sdr_rgb: Rgb,
}

impl Bt2446CReferenceOutput {
    /// Проверяет, что все exposed intermediate values are finite.
    fn intermediates_are_finite(self) -> bool {
        self.normalized_yuv.y.is_finite()
            && self.normalized_yuv.cb.is_finite()
            && self.normalized_yuv.cr.is_finite()
            && self.nonlinear_bt2020_rgb.is_finite()
            && self.linear_hdr_rgb.is_finite()
            && self.crosstalk_hdr_rgb.is_finite()
            && self.hdr_xyz.is_finite()
            && self.hdr_chromaticity.is_finite()
            && self.tone_mapped_sdr_luminance_nits.is_finite()
            && self.crosstalk_sdr_rgb.is_finite()
            && self.linear_sdr_rgb_before_final_clamp.is_finite()
            && self.encoded_sdr_rgb.is_finite()
    }
}

/// Runs the isolated HDR-to-SDR CPU reference for one P010 sample.
fn convert_p010_sample(input: &Bt2446CReferenceInput) -> Result<Bt2446CReferenceOutput> {
    validate_reference_input(input)?;

    let normalized_yuv = normalize_p010_code_values(input.sample, input.color.range)?;
    let nonlinear_bt2020_rgb = bt2020_yuv_to_rgb(normalized_yuv);
    let linear_hdr_rgb = apply_hdr_eotf(
        nonlinear_bt2020_rgb,
        input.color.transfer,
        input.settings.hdr_to_sdr.hdr_reference_peak_nits as f64,
    )?;

    convert_linear_hdr_rgb(
        linear_hdr_rgb,
        input.settings,
        normalized_yuv,
        nonlinear_bt2020_rgb,
    )
}

/// Runs BT.2446-C after transfer decode, useful for compact reference vectors.
fn convert_linear_hdr_rgb(
    linear_hdr_rgb: Rgb,
    settings: Bt2446CReferenceSettings,
    normalized_yuv: NormalizedYuv,
    nonlinear_bt2020_rgb: Rgb,
) -> Result<Bt2446CReferenceOutput> {
    ensure!(
        linear_hdr_rgb.is_finite(),
        "BT.2446-C reference requires finite linear HDR RGB"
    );
    validate_reference_settings(settings)?;

    let crosstalk_hdr_rgb = apply_crosstalk(linear_hdr_rgb, settings.crosstalk_alpha);
    let hdr_xyz = bt2020_linear_rgb_to_xyz(crosstalk_hdr_rgb);
    let hdr_chromaticity = xyz_to_chromaticity(hdr_xyz);
    let tone_mapped_sdr_luminance_nits = bt2446c_tone_map_luminance_nits(hdr_xyz.y);
    let sdr_xyz =
        xyz_from_luminance_and_chromaticity(tone_mapped_sdr_luminance_nits, hdr_chromaticity);
    let crosstalk_sdr_rgb = xyz_to_bt709_linear_rgb(sdr_xyz);
    let linear_sdr_rgb_before_final_clamp =
        apply_inverse_crosstalk(crosstalk_sdr_rgb, settings.crosstalk_alpha)?;
    let encoded_sdr_rgb = encode_sdr_bt709_output(
        linear_sdr_rgb_before_final_clamp,
        settings.hdr_to_sdr.sdr_reference_white_nits as f64,
    )?;

    Ok(Bt2446CReferenceOutput {
        normalized_yuv,
        nonlinear_bt2020_rgb,
        linear_hdr_rgb,
        crosstalk_hdr_rgb,
        hdr_xyz,
        hdr_chromaticity,
        tone_mapped_sdr_luminance_nits,
        crosstalk_sdr_rgb,
        linear_sdr_rgb_before_final_clamp,
        encoded_sdr_rgb,
    })
}

/// Validates the full reference input contract before any math runs.
fn validate_reference_input(input: &Bt2446CReferenceInput) -> Result<()> {
    validate_reference_settings(input.settings)?;
    validate_hdr_color_metadata(&input.color)?;
    validate_p010_code_values(input.sample)?;

    Ok(())
}

/// Validates settings that materially affect reference vectors.
fn validate_reference_settings(settings: Bt2446CReferenceSettings) -> Result<()> {
    let hdr_to_sdr = settings.hdr_to_sdr;

    ensure!(
        hdr_to_sdr.enabled,
        "HDR-to-SDR reference requires enabled settings"
    );
    ensure!(
        hdr_to_sdr.operator == HdrToneMappingOperator::Bt2446C,
        "HDR-to-SDR reference requires BT.2446-C operator"
    );
    ensure!(
        hdr_to_sdr.output_mode == HdrOutputMode::SdrBt709Only,
        "HDR-to-SDR reference only supports SDR BT.709 output"
    );
    ensure!(
        hdr_to_sdr.sdr_reference_white_nits.is_finite()
            && hdr_to_sdr.sdr_reference_white_nits > 0.0,
        "HDR-to-SDR reference requires positive finite SDR white"
    );
    ensure!(
        hdr_to_sdr.hdr_reference_peak_nits.is_finite()
            && hdr_to_sdr.hdr_reference_peak_nits > hdr_to_sdr.sdr_reference_white_nits,
        "HDR-to-SDR reference requires HDR peak above SDR white"
    );
    ensure!(
        settings.crosstalk_alpha.is_finite()
            && (0.0..(1.0 / 3.0)).contains(&settings.crosstalk_alpha),
        "BT.2446-C crosstalk alpha must be finite and in [0, 1/3)"
    );

    Ok(())
}

/// Validates strict Phase 10 HDR metadata for the reference path.
fn validate_hdr_color_metadata(color: &VideoColorMetadata) -> Result<()> {
    ensure!(
        matches!(color.range, ColorRange::Limited | ColorRange::Full),
        "BT.2446-C reference requires explicit limited/full color range"
    );
    ensure!(
        color.matrix == MatrixCoefficients::Bt2020,
        "BT.2446-C reference requires BT.2020 matrix"
    );
    ensure!(
        color.primaries == ColorPrimaries::Bt2020,
        "BT.2446-C reference requires BT.2020 primaries"
    );
    ensure!(
        matches!(color.transfer, TransferFunction::Pq | TransferFunction::Hlg),
        "BT.2446-C reference requires PQ or HLG transfer"
    );

    if let Some(hdr_metadata) = &color.hdr_metadata {
        ensure!(
            hdr_metadata.color_primaries == color.primaries,
            "BT.2446-C reference HDR metadata primaries mismatch"
        );
        ensure!(
            hdr_metadata.transfer_function == color.transfer,
            "BT.2446-C reference HDR metadata transfer mismatch"
        );
    }

    Ok(())
}

/// Validates P010 code values without silently clipping test inputs.
fn validate_p010_code_values(sample: P010CodeValues) -> Result<()> {
    for (label, code_value) in [("Y", sample.y), ("Cb", sample.cb), ("Cr", sample.cr)] {
        ensure!(
            code_value.is_finite()
                && (P010_10BIT_MIN_CODE_VALUE..=P010_10BIT_MAX_CODE_VALUE).contains(&code_value),
            "P010 {label} code value must be finite and inside 10-bit range"
        );
    }

    Ok(())
}

/// Normalizes 10-bit P010 code values according to explicit color range.
fn normalize_p010_code_values(sample: P010CodeValues, range: ColorRange) -> Result<NormalizedYuv> {
    match range {
        ColorRange::Limited => Ok(NormalizedYuv {
            y: (sample.y - P010_LIMITED_LUMA_BLACK_CODE_VALUE)
                / (P010_LIMITED_LUMA_WHITE_CODE_VALUE - P010_LIMITED_LUMA_BLACK_CODE_VALUE),
            cb: (sample.cb - P010_CHROMA_CENTER_CODE_VALUE)
                / (P010_LIMITED_CHROMA_MAX_CODE_VALUE - P010_LIMITED_CHROMA_MIN_CODE_VALUE),
            cr: (sample.cr - P010_CHROMA_CENTER_CODE_VALUE)
                / (P010_LIMITED_CHROMA_MAX_CODE_VALUE - P010_LIMITED_CHROMA_MIN_CODE_VALUE),
        }),
        ColorRange::Full => Ok(NormalizedYuv {
            y: (sample.y - P010_10BIT_MIN_CODE_VALUE)
                / (P010_10BIT_MAX_CODE_VALUE - P010_10BIT_MIN_CODE_VALUE),
            cb: (sample.cb - P010_CHROMA_CENTER_CODE_VALUE)
                / (P010_10BIT_MAX_CODE_VALUE - P010_10BIT_MIN_CODE_VALUE),
            cr: (sample.cr - P010_CHROMA_CENTER_CODE_VALUE)
                / (P010_10BIT_MAX_CODE_VALUE - P010_10BIT_MIN_CODE_VALUE),
        }),
        ColorRange::Unknown => bail!("P010 range must be explicit before reference conversion"),
    }
}

/// Converts normalized BT.2020 non-constant-luminance YCbCr to non-linear RGB.
fn bt2020_yuv_to_rgb(yuv: NormalizedYuv) -> Rgb {
    let red = yuv.y + 2.0 * (1.0 - BT2020_KR) * yuv.cr;
    let blue = yuv.y + 2.0 * (1.0 - BT2020_KB) * yuv.cb;
    let green = (yuv.y - BT2020_KR * red - BT2020_KB * blue) / BT2020_KG;

    Rgb::new(red, green, blue)
}

/// Applies the selected HDR EOTF to non-linear BT.2020 RGB.
fn apply_hdr_eotf(rgb: Rgb, transfer: TransferFunction, hlg_peak_nits: f64) -> Result<Rgb> {
    validate_transfer_domain(rgb)?;

    match transfer {
        TransferFunction::Pq => Ok(Rgb::new(
            pq_eotf_nits(rgb.red),
            pq_eotf_nits(rgb.green),
            pq_eotf_nits(rgb.blue),
        )),
        TransferFunction::Hlg => hlg_eotf_rgb_nits(rgb, hlg_peak_nits),
        other_transfer => bail!("unsupported HDR transfer in reference: {other_transfer:?}"),
    }
}

/// Rejects transfer inputs outside the defined [0, 1] signal range instead of clamping them.
fn validate_transfer_domain(rgb: Rgb) -> Result<()> {
    for (label, channel) in [("red", rgb.red), ("green", rgb.green), ("blue", rgb.blue)] {
        ensure!(
            channel.is_finite() && (0.0..=1.0).contains(&channel),
            "HDR transfer input channel {label} must be finite and inside [0, 1]"
        );
    }

    Ok(())
}

/// Applies the ST 2084 / PQ EOTF and returns absolute luminance in nits.
fn pq_eotf_nits(encoded_value: f64) -> f64 {
    let encoded_root = encoded_value.powf(1.0 / PQ_M2);
    let numerator = (encoded_root - PQ_C1).max(0.0);
    let denominator = PQ_C2 - PQ_C3 * encoded_root;
    let normalized_luminance = (numerator / denominator).powf(1.0 / PQ_M1);

    PQ_REFERENCE_PEAK_NITS * normalized_luminance
}

/// Applies the inverse ST 2084 / PQ EOTF, useful for exact reference samples.
fn pq_inverse_eotf_from_nits(luminance_nits: f64) -> f64 {
    let normalized_luminance = (luminance_nits / PQ_REFERENCE_PEAK_NITS).max(0.0);
    let powered_luminance = normalized_luminance.powf(PQ_M1);

    ((PQ_C1 + PQ_C2 * powered_luminance) / (1.0 + PQ_C3 * powered_luminance)).powf(PQ_M2)
}

/// Applies the BT.2100 HLG EOTF with zero black level.
fn hlg_eotf_rgb_nits(encoded_rgb: Rgb, peak_nits: f64) -> Result<Rgb> {
    ensure!(
        peak_nits.is_finite() && peak_nits > 0.0,
        "HLG EOTF requires positive finite peak luminance"
    );

    let scene_rgb = Rgb::new(
        hlg_inverse_oetf(encoded_rgb.red),
        hlg_inverse_oetf(encoded_rgb.green),
        hlg_inverse_oetf(encoded_rgb.blue),
    );
    let scene_luma =
        BT2020_KR * scene_rgb.red + BT2020_KG * scene_rgb.green + BT2020_KB * scene_rgb.blue;

    if scene_luma <= 0.0 {
        return Ok(Rgb::new(0.0, 0.0, 0.0));
    }

    let system_gamma_scale = scene_luma.powf(HLG_REFERENCE_SYSTEM_GAMMA - 1.0);

    Ok(Rgb::new(
        peak_nits * system_gamma_scale * scene_rgb.red,
        peak_nits * system_gamma_scale * scene_rgb.green,
        peak_nits * system_gamma_scale * scene_rgb.blue,
    ))
}

/// Applies the inverse HLG reference OETF.
fn hlg_inverse_oetf(encoded_value: f64) -> f64 {
    if encoded_value <= 0.5 {
        encoded_value * encoded_value / 3.0
    } else {
        ((encoded_value - HLG_C) / HLG_A).exp().mul_add(1.0, HLG_B) / 12.0
    }
}

/// Applies the BT.2446-C crosstalk matrix.
fn apply_crosstalk(rgb: Rgb, alpha: f64) -> Rgb {
    let self_weight = 1.0 - 2.0 * alpha;

    Rgb::new(
        self_weight * rgb.red + alpha * rgb.green + alpha * rgb.blue,
        alpha * rgb.red + self_weight * rgb.green + alpha * rgb.blue,
        alpha * rgb.red + alpha * rgb.green + self_weight * rgb.blue,
    )
}

/// Converts BT.2020 linear RGB to CIE XYZ.
fn bt2020_linear_rgb_to_xyz(rgb: Rgb) -> Xyz {
    let channels = rgb.channels();
    let xyz = multiply_matrix_vector(BT2020_LINEAR_RGB_TO_XYZ, channels);

    Xyz {
        x: xyz[0],
        y: xyz[1],
        z: xyz[2],
    }
}

/// Converts CIE XYZ to xy chromaticity, using D65 only for true black.
fn xyz_to_chromaticity(xyz: Xyz) -> Chromaticity {
    let tristimulus_sum = xyz.x + xyz.y + xyz.z;

    if tristimulus_sum.abs() <= f64::EPSILON {
        return Chromaticity {
            x: D65_WHITE_X,
            y: D65_WHITE_Y,
        };
    }

    Chromaticity {
        x: xyz.x / tristimulus_sum,
        y: xyz.y / tristimulus_sum,
    }
}

/// Applies the BT.2446-C luminance tone curve.
fn bt2446c_tone_map_luminance_nits(hdr_luminance_nits: f64) -> f64 {
    let hdr_inflection_nits = BT2446C_SDR_INFLECTION_NITS / BT2446C_K1;

    if hdr_luminance_nits < hdr_inflection_nits {
        BT2446C_K1 * hdr_luminance_nits
    } else {
        BT2446C_K2 * (hdr_luminance_nits / hdr_inflection_nits - BT2446C_K3).ln() + BT2446C_K4
    }
}

/// Rebuilds XYZ from tone-mapped luminance and preserved xy chromaticity.
fn xyz_from_luminance_and_chromaticity(luminance_nits: f64, chromaticity: Chromaticity) -> Xyz {
    if luminance_nits == 0.0 {
        return Xyz {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
    }

    Xyz {
        x: chromaticity.x / chromaticity.y * luminance_nits,
        y: luminance_nits,
        z: (1.0 - chromaticity.x - chromaticity.y) / chromaticity.y * luminance_nits,
    }
}

/// Converts CIE XYZ to BT.709 linear RGB.
fn xyz_to_bt709_linear_rgb(xyz: Xyz) -> Rgb {
    let rgb = multiply_matrix_vector(XYZ_TO_BT709_LINEAR_RGB, [xyz.x, xyz.y, xyz.z]);

    Rgb::new(rgb[0], rgb[1], rgb[2])
}

/// Applies the inverse BT.2446-C crosstalk matrix.
fn apply_inverse_crosstalk(rgb: Rgb, alpha: f64) -> Result<Rgb> {
    let denominator = 1.0 - 3.0 * alpha;

    ensure!(
        denominator.is_finite() && denominator > 0.0,
        "BT.2446-C inverse crosstalk requires alpha below 1/3"
    );

    Ok(Rgb::new(
        ((1.0 - alpha) * rgb.red - alpha * rgb.green - alpha * rgb.blue) / denominator,
        (-alpha * rgb.red + (1.0 - alpha) * rgb.green - alpha * rgb.blue) / denominator,
        (-alpha * rgb.red - alpha * rgb.green + (1.0 - alpha) * rgb.blue) / denominator,
    ))
}

/// Applies inverse BT.1886 and clamps only at this final SDR output stage.
fn encode_sdr_bt709_output(linear_rgb_nits: Rgb, sdr_reference_white_nits: f64) -> Result<Rgb> {
    ensure!(
        sdr_reference_white_nits.is_finite() && sdr_reference_white_nits > 0.0,
        "SDR output transfer requires positive finite reference white"
    );

    Ok(Rgb::new(
        inverse_bt1886_oetf(linear_rgb_nits.red, sdr_reference_white_nits),
        inverse_bt1886_oetf(linear_rgb_nits.green, sdr_reference_white_nits),
        inverse_bt1886_oetf(linear_rgb_nits.blue, sdr_reference_white_nits),
    ))
}

/// Applies inverse BT.1886 with final output-domain clamp.
fn inverse_bt1886_oetf(linear_nits: f64, reference_white_nits: f64) -> f64 {
    (linear_nits / reference_white_nits)
        .clamp(0.0, 1.0)
        .powf(1.0 / BT1886_REFERENCE_GAMMA)
}

/// Multiplies a 3x3 matrix by a 3-channel vector.
fn multiply_matrix_vector(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tight tolerance for deterministic f64 reference checks.
    const STRICT_TOLERANCE: f64 = 0.000_000_001;

    /// Practical tolerance for rounded standard matrices/constants.
    const MATRIX_TOLERANCE: f64 = 0.000_01;

    /// WGSL shader source used to lock shader constants to this CPU reference.
    const P010_SHADER_SOURCE: &str = include_str!("../shaders/p010_bt2446c_to_sdr.wgsl");

    #[test]
    fn shader_bt2446c_constants_match_cpu_reference_constants() {
        assert_shader_const_matches("P010_10BIT_MAX_CODE_VALUE", P010_10BIT_MAX_CODE_VALUE);
        assert_shader_const_matches("BT2020_KR", BT2020_KR);
        assert_shader_const_matches("BT2020_KB", BT2020_KB);
        assert_shader_const_matches("BT2020_KG", BT2020_KG);
        assert_shader_const_matches("PQ_M1", PQ_M1);
        assert_shader_const_matches("PQ_M2", PQ_M2);
        assert_shader_const_matches("PQ_C1", PQ_C1);
        assert_shader_const_matches("PQ_C2", PQ_C2);
        assert_shader_const_matches("PQ_C3", PQ_C3);
        assert_shader_const_matches("PQ_REFERENCE_PEAK_NITS", PQ_REFERENCE_PEAK_NITS);
        assert_shader_const_matches("HLG_A", HLG_A);
        assert_shader_const_matches("HLG_B", HLG_B);
        assert_shader_const_matches("HLG_C", HLG_C);
        assert_shader_const_matches("HLG_REFERENCE_SYSTEM_GAMMA", HLG_REFERENCE_SYSTEM_GAMMA);
        assert_shader_const_matches(
            "BT2446C_REFERENCE_CROSSTALK_ALPHA",
            BT2446C_REFERENCE_CROSSTALK_ALPHA,
        );
        assert_shader_const_matches("BT2446C_K1", BT2446C_K1);
        assert_shader_const_matches("BT2446C_K2", BT2446C_K2);
        assert_shader_const_matches("BT2446C_K3", BT2446C_K3);
        assert_shader_const_matches("BT2446C_K4", BT2446C_K4);
        assert_shader_const_matches("BT2446C_SDR_INFLECTION_NITS", BT2446C_SDR_INFLECTION_NITS);
        assert_shader_const_matches("BT1886_REFERENCE_GAMMA", BT1886_REFERENCE_GAMMA);
        assert_shader_const_matches("D65_WHITE_X", D65_WHITE_X);
        assert_shader_const_matches("D65_WHITE_Y", D65_WHITE_Y);
        assert_shader_vec3_matches("BT2020_LINEAR_RGB_TO_XYZ_ROW0", BT2020_LINEAR_RGB_TO_XYZ[0]);
        assert_shader_vec3_matches("BT2020_LINEAR_RGB_TO_XYZ_ROW1", BT2020_LINEAR_RGB_TO_XYZ[1]);
        assert_shader_vec3_matches("BT2020_LINEAR_RGB_TO_XYZ_ROW2", BT2020_LINEAR_RGB_TO_XYZ[2]);
        assert_shader_vec3_matches("XYZ_TO_BT709_LINEAR_RGB_ROW0", XYZ_TO_BT709_LINEAR_RGB[0]);
        assert_shader_vec3_matches("XYZ_TO_BT709_LINEAR_RGB_ROW1", XYZ_TO_BT709_LINEAR_RGB[1]);
        assert_shader_vec3_matches("XYZ_TO_BT709_LINEAR_RGB_ROW2", XYZ_TO_BT709_LINEAR_RGB[2]);
    }

    #[test]
    fn pq_eotf_known_points_match_st2084_reference_values() {
        assert_close(pq_eotf_nits(0.0), 0.0, STRICT_TOLERANCE);
        assert_close(pq_eotf_nits(0.508_078_421_517_399), 100.0, MATRIX_TOLERANCE);
        assert_close(
            pq_eotf_nits(0.751_827_096_247_041),
            1_000.0,
            MATRIX_TOLERANCE,
        );
        assert_close(pq_eotf_nits(1.0), 10_000.0, STRICT_TOLERANCE);
    }

    #[test]
    fn hlg_eotf_known_points_match_bt2100_reference_values() {
        assert_close(hlg_eotf_achromatic_nits(0.0), 0.0, STRICT_TOLERANCE);
        assert_close(
            hlg_eotf_achromatic_nits(0.5),
            50.697_028_491,
            MATRIX_TOLERANCE,
        );
        assert_close(
            hlg_eotf_achromatic_nits(0.75),
            203.152_145_938,
            MATRIX_TOLERANCE,
        );
        assert_close(hlg_eotf_achromatic_nits(1.0), 1_000.0, 0.000_1);
    }

    #[test]
    fn bt2020_yuv_to_rgb_matrix_known_samples_match_reference_values() {
        assert_rgb_close(
            bt2020_yuv_to_rgb(NormalizedYuv {
                y: 0.5,
                cb: 0.0,
                cr: 0.0,
            }),
            Rgb::new(0.5, 0.5, 0.5),
            STRICT_TOLERANCE,
        );
        assert_rgb_close(
            bt2020_yuv_to_rgb(normalized_bt2020_yuv_from_rgb(Rgb::new(1.0, 0.0, 0.0))),
            Rgb::new(1.0, 0.0, 0.0),
            STRICT_TOLERANCE,
        );
        assert_rgb_close(
            bt2020_yuv_to_rgb(normalized_bt2020_yuv_from_rgb(Rgb::new(0.0, 1.0, 0.0))),
            Rgb::new(0.0, 1.0, 0.0),
            STRICT_TOLERANCE,
        );
        assert_rgb_close(
            bt2020_yuv_to_rgb(normalized_bt2020_yuv_from_rgb(Rgb::new(0.0, 0.0, 1.0))),
            Rgb::new(0.0, 0.0, 1.0),
            STRICT_TOLERANCE,
        );
    }

    #[test]
    fn bt2446c_neutral_reference_vectors_match_expected_values() {
        let settings = Bt2446CReferenceSettings::default_reference();
        let cases = [
            (
                Rgb::new(100.0, 100.0, 100.0),
                Rgb::new(0.879_130_957, 0.879_115_981, 0.879_099_531),
            ),
            (
                Rgb::new(203.0, 203.0, 203.0),
                Rgb::new(0.960_007_104, 0.959_990_750, 0.959_972_786),
            ),
            (Rgb::new(1_000.0, 1_000.0, 1_000.0), Rgb::new(1.0, 1.0, 1.0)),
        ];

        for (linear_hdr_rgb, expected_encoded_rgb) in cases {
            let reference_output =
                convert_direct_linear_sample(linear_hdr_rgb, settings).expect("valid vector");

            assert!(reference_output.intermediates_are_finite());
            assert_rgb_close(
                reference_output.encoded_sdr_rgb,
                expected_encoded_rgb,
                MATRIX_TOLERANCE,
            );
        }
    }

    #[test]
    fn bt2446c_colored_reference_vector_matches_expected_values() {
        let reference_output = convert_direct_linear_sample(
            Rgb::new(203.0, 100.0, 50.0),
            Bt2446CReferenceSettings::default_reference(),
        )
        .expect("valid colored vector");

        assert!(reference_output.intermediates_are_finite());
        assert_rgb_close(
            reference_output.encoded_sdr_rgb,
            Rgb::new(1.0, 0.832_203_632, 0.623_419_858),
            MATRIX_TOLERANCE,
        );
        assert!(
            reference_output.linear_sdr_rgb_before_final_clamp.red > 100.0,
            "red channel should prove final-stage-only highlight clamp"
        );
    }

    #[test]
    fn p010_pq_reference_pipeline_uses_expected_intermediate_values() {
        let encoded_reference_white = pq_inverse_eotf_from_nits(203.0);
        let sample = limited_p010_sample_from_normalized_yuv(NormalizedYuv {
            y: encoded_reference_white,
            cb: 0.0,
            cr: 0.0,
        });
        let input = Bt2446CReferenceInput {
            sample,
            color: strict_hdr_color(TransferFunction::Pq, ColorRange::Limited),
            settings: Bt2446CReferenceSettings::default_reference(),
        };

        let reference_output = convert_p010_sample(&input).expect("valid PQ reference sample");

        assert!(reference_output.intermediates_are_finite());
        assert_rgb_close(
            reference_output.linear_hdr_rgb,
            Rgb::new(203.0, 203.0, 203.0),
            MATRIX_TOLERANCE,
        );
        assert_rgb_close(
            reference_output.encoded_sdr_rgb,
            Rgb::new(0.960_007_104, 0.959_990_750, 0.959_972_786),
            MATRIX_TOLERANCE,
        );
    }

    #[test]
    fn p010_hlg_reference_pipeline_uses_expected_intermediate_values() {
        let sample = limited_p010_sample_from_normalized_yuv(NormalizedYuv {
            y: 0.75,
            cb: 0.0,
            cr: 0.0,
        });
        let input = Bt2446CReferenceInput {
            sample,
            color: strict_hdr_color(TransferFunction::Hlg, ColorRange::Limited),
            settings: Bt2446CReferenceSettings::default_reference(),
        };

        let reference_output = convert_p010_sample(&input).expect("valid HLG reference sample");

        assert!(reference_output.intermediates_are_finite());
        assert_rgb_close(
            reference_output.linear_hdr_rgb,
            Rgb::new(203.152_145_938, 203.152_145_938, 203.152_145_938),
            MATRIX_TOLERANCE,
        );
        assert_rgb_close(
            reference_output.encoded_sdr_rgb,
            Rgb::new(0.960_074_102, 0.960_057_747, 0.960_039_782),
            MATRIX_TOLERANCE,
        );
    }

    #[test]
    fn output_values_clamp_only_at_final_sdr_transfer_stage() {
        let reference_output = convert_direct_linear_sample(
            Rgb::new(203.0, 0.0, 0.0),
            Bt2446CReferenceSettings::default_reference(),
        )
        .expect("valid red BT.2446-C reference sample");

        assert!(reference_output.linear_sdr_rgb_before_final_clamp.red > 100.0);
        assert!(reference_output.linear_sdr_rgb_before_final_clamp.green < 0.0);
        assert!(reference_output.linear_sdr_rgb_before_final_clamp.blue < 0.0);
        assert_rgb_close(
            reference_output.encoded_sdr_rgb,
            Rgb::new(1.0, 0.0, 0.0),
            MATRIX_TOLERANCE,
        );
    }

    #[test]
    fn sdr_bt709_output_transfer_known_points_match_inverse_bt1886() {
        assert_close(inverse_bt1886_oetf(0.0, 100.0), 0.0, STRICT_TOLERANCE);
        assert_close(
            inverse_bt1886_oetf(18.0, 100.0),
            0.489_437_089,
            MATRIX_TOLERANCE,
        );
        assert_close(inverse_bt1886_oetf(100.0, 100.0), 1.0, STRICT_TOLERANCE);
        assert_close(inverse_bt1886_oetf(118.0, 100.0), 1.0, STRICT_TOLERANCE);
        assert_close(inverse_bt1886_oetf(-1.0, 100.0), 0.0, STRICT_TOLERANCE);
    }

    #[test]
    fn cpu_reference_rejects_invalid_metadata_and_settings() {
        let valid_sample = limited_p010_sample_from_normalized_yuv(NormalizedYuv {
            y: 0.75,
            cb: 0.0,
            cr: 0.0,
        });

        let mut unknown_range_color = strict_hdr_color(TransferFunction::Pq, ColorRange::Limited);
        unknown_range_color.range = ColorRange::Unknown;
        let mut bt709_matrix_color = strict_hdr_color(TransferFunction::Pq, ColorRange::Limited);
        bt709_matrix_color.matrix = MatrixCoefficients::Bt709;
        let mut bt709_primaries_color = strict_hdr_color(TransferFunction::Pq, ColorRange::Limited);
        bt709_primaries_color.primaries = ColorPrimaries::Bt709;
        let mut bt709_transfer_color = strict_hdr_color(TransferFunction::Pq, ColorRange::Limited);
        bt709_transfer_color.transfer = TransferFunction::Bt709;

        for (invalid_color, expected_error_fragment) in [
            (unknown_range_color, "limited/full color range"),
            (bt709_matrix_color, "BT.2020 matrix"),
            (bt709_primaries_color, "BT.2020 primaries"),
            (bt709_transfer_color, "PQ or HLG transfer"),
        ] {
            let invalid_color_input = Bt2446CReferenceInput {
                sample: valid_sample,
                color: invalid_color,
                settings: Bt2446CReferenceSettings::default_reference(),
            };
            let invalid_color_error = convert_p010_sample(&invalid_color_input)
                .expect_err("invalid strict HDR metadata must be rejected");

            assert!(
                invalid_color_error
                    .to_string()
                    .contains(expected_error_fragment),
                "expected `{expected_error_fragment}` in metadata error, got: {invalid_color_error}"
            );
        }

        let invalid_peak_settings = Bt2446CReferenceSettings {
            hdr_to_sdr: HdrToSdrSettings {
                hdr_reference_peak_nits: 100.0,
                ..HdrToSdrSettings::bt2446_c_sdr_bt709()
            },
            ..Bt2446CReferenceSettings::default_reference()
        };
        let invalid_settings_input = Bt2446CReferenceInput {
            sample: valid_sample,
            color: strict_hdr_color(TransferFunction::Pq, ColorRange::Limited),
            settings: invalid_peak_settings,
        };
        let invalid_settings_error = convert_p010_sample(&invalid_settings_input)
            .expect_err("HDR peak at SDR white must be rejected");

        assert!(
            invalid_settings_error
                .to_string()
                .contains("HDR peak above SDR white"),
            "unexpected settings error: {invalid_settings_error}"
        );

        let invalid_alpha_settings = Bt2446CReferenceSettings {
            crosstalk_alpha: 1.0 / 3.0,
            ..Bt2446CReferenceSettings::default_reference()
        };
        let invalid_alpha_input = Bt2446CReferenceInput {
            sample: valid_sample,
            color: strict_hdr_color(TransferFunction::Hlg, ColorRange::Limited),
            settings: invalid_alpha_settings,
        };
        let invalid_alpha_error = convert_p010_sample(&invalid_alpha_input)
            .expect_err("singular crosstalk matrix must be rejected");

        assert!(
            invalid_alpha_error.to_string().contains("crosstalk alpha"),
            "unexpected alpha error: {invalid_alpha_error}"
        );
    }

    /// Converts an achromatic HLG signal to nits for transfer known-point tests.
    fn hlg_eotf_achromatic_nits(encoded_value: f64) -> f64 {
        hlg_eotf_rgb_nits(
            Rgb::new(encoded_value, encoded_value, encoded_value),
            HdrToSdrSettings::bt2446_c_sdr_bt709().hdr_reference_peak_nits as f64,
        )
        .expect("valid HLG achromatic input")
        .red
    }

    /// Runs BT.2446-C directly from linear HDR RGB for compact standard vectors.
    fn convert_direct_linear_sample(
        linear_hdr_rgb: Rgb,
        settings: Bt2446CReferenceSettings,
    ) -> Result<Bt2446CReferenceOutput> {
        convert_linear_hdr_rgb(
            linear_hdr_rgb,
            settings,
            NormalizedYuv {
                y: 0.0,
                cb: 0.0,
                cr: 0.0,
            },
            Rgb::new(0.0, 0.0, 0.0),
        )
    }

    /// Builds strict HDR metadata accepted by Phase 10 reference validation.
    fn strict_hdr_color(transfer: TransferFunction, range: ColorRange) -> VideoColorMetadata {
        VideoColorMetadata {
            range,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer,
            hdr_metadata: Some(HdrMetadata {
                color_primaries: ColorPrimaries::Bt2020,
                transfer_function: transfer,
                max_luminance_nits: Some(1_000.0),
                min_luminance_nits: Some(0.01),
                max_content_light_level_nits: Some(1_000),
                max_frame_average_light_level_nits: Some(400),
            }),
            origin: ColorMetadataOrigin::Bitstream,
            confidence: ColorMetadataConfidence::Confirmed,
        }
    }

    /// Converts RGB to normalized BT.2020 YCbCr for matrix round-trip vectors.
    fn normalized_bt2020_yuv_from_rgb(rgb: Rgb) -> NormalizedYuv {
        let y = BT2020_KR * rgb.red + BT2020_KG * rgb.green + BT2020_KB * rgb.blue;
        let cb = (rgb.blue - y) / (2.0 * (1.0 - BT2020_KB));
        let cr = (rgb.red - y) / (2.0 * (1.0 - BT2020_KR));

        NormalizedYuv { y, cb, cr }
    }

    /// Converts normalized YCbCr into fractional limited-range P010 code values.
    fn limited_p010_sample_from_normalized_yuv(yuv: NormalizedYuv) -> P010CodeValues {
        P010CodeValues {
            y: P010_LIMITED_LUMA_BLACK_CODE_VALUE
                + yuv.y * (P010_LIMITED_LUMA_WHITE_CODE_VALUE - P010_LIMITED_LUMA_BLACK_CODE_VALUE),
            cb: P010_CHROMA_CENTER_CODE_VALUE
                + yuv.cb
                    * (P010_LIMITED_CHROMA_MAX_CODE_VALUE - P010_LIMITED_CHROMA_MIN_CODE_VALUE),
            cr: P010_CHROMA_CENTER_CODE_VALUE
                + yuv.cr
                    * (P010_LIMITED_CHROMA_MAX_CODE_VALUE - P010_LIMITED_CHROMA_MIN_CODE_VALUE),
        }
    }

    /// Checks one scalar with explicit tolerance and useful panic text.
    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "actual {actual:.12} differs from expected {expected:.12} by more than {tolerance}"
        );
    }

    /// Checks RGB channels with explicit tolerance and useful panic text.
    fn assert_rgb_close(actual: Rgb, expected: Rgb, tolerance: f64) {
        for (channel_index, (actual_channel, expected_channel)) in actual
            .channels()
            .into_iter()
            .zip(expected.channels())
            .enumerate()
        {
            assert!(
                (actual_channel - expected_channel).abs() <= tolerance,
                "RGB channel {channel_index} actual {actual_channel:.12} expected {expected_channel:.12}"
            );
        }
    }

    /// Compares one scalar WGSL constant against the CPU reference value.
    fn assert_shader_const_matches(const_name: &str, expected: f64) {
        assert_close(shader_f32_const(const_name), expected, MATRIX_TOLERANCE);
    }

    /// Compares one WGSL `vec3<f32>` constant against the CPU reference row.
    fn assert_shader_vec3_matches(const_name: &str, expected: [f64; 3]) {
        let actual = shader_vec3_const(const_name);

        for (channel_index, (actual_channel, expected_channel)) in
            actual.into_iter().zip(expected).enumerate()
        {
            assert!(
                (actual_channel - expected_channel).abs() <= MATRIX_TOLERANCE,
                "WGSL vec3 const `{const_name}` channel {channel_index} actual {actual_channel:.12} expected {expected_channel:.12}"
            );
        }
    }

    /// Reads a scalar `const name: f32 = value;` from the WGSL shader source.
    fn shader_f32_const(const_name: &str) -> f64 {
        let prefix = format!("const {const_name}: f32 = ");
        let line = shader_const_line(&prefix, const_name);
        let value = line
            .trim_start_matches(&prefix)
            .trim_end_matches(';')
            .replace('_', "");

        value
            .parse()
            .unwrap_or_else(|error| panic!("WGSL const `{const_name}` is not f32: {error}"))
    }

    /// Reads a `const name: vec3<f32> = vec3<f32>(...);` from the WGSL shader source.
    fn shader_vec3_const(const_name: &str) -> [f64; 3] {
        let prefix = format!("const {const_name}: vec3<f32> = vec3<f32>(");
        let line = shader_const_line(&prefix, const_name);
        let values = line
            .trim_start_matches(&prefix)
            .trim_end_matches(");")
            .split(',')
            .map(|value| {
                value.trim().parse::<f64>().unwrap_or_else(|error| {
                    panic!("WGSL vec3 const `{const_name}` is invalid: {error}")
                })
            })
            .collect::<Vec<_>>();

        assert_eq!(
            values.len(),
            3,
            "WGSL vec3 const `{const_name}` must contain three values"
        );

        [values[0], values[1], values[2]]
    }

    /// Finds a WGSL const declaration by exact prefix.
    fn shader_const_line(prefix: &str, const_name: &str) -> &'static str {
        P010_SHADER_SOURCE
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("WGSL const `{const_name}` not found"))
    }
}
