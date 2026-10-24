//! Уровни возможностей на строках живых машин разведки 00 и на границах.

use super::dma_buf::query_support as dma_buf_support;
use super::*;
use crate::context::display_registry::{DisplayLease, DisplayRegistry};
use crate::context::driver::{DisplayId, DmaBufModifier, EglErrorCode, NativeDisplay};
use crate::test_support::fake_egl::{FakeBehavior, FakeEgl};

const DRM_FORMAT_MOD_LINEAR: u64 = 0;
const I915_FORMAT_MOD_X_TILED: u64 = (1 << 56) | 1;
const AMD_TILED_MODIFIER: u64 = 0x0200_0000_1040_1b04;
const DRM_FORMAT_NV12: DrmFourcc = DrmFourcc::from_code(*b"NV12");

fn sampler(modifier: u64) -> DmaBufModifier {
    DmaBufModifier {
        modifier,
        external_only: false,
    }
}

fn external(modifier: u64) -> DmaBufModifier {
    DmaBufModifier {
        modifier,
        external_only: true,
    }
}

/// Ironlake (T410, crocus): ES 2.0, все расширения таблицы 00; R8/GR88/ABGR8888 с LINEAR и
/// X_TILED как `sampler2D`, NV12 только external (живой вывод примера, сессия 03).
fn ironlake_report() -> DriverReport {
    DriverReport {
        gl_version_string: "OpenGL ES 2.0 Mesa 26.2.4".to_owned(),
        reported_gles_version: GlesVersion { major: 2, minor: 0 },
        gl_vendor: "Intel".to_owned(),
        gl_renderer: "Mesa Intel(R) HD Graphics (ILK)".to_owned(),
        shading_language_version: "OpenGL ES GLSL ES 1.0.16".to_owned(),
        gl_extensions: ExtensionSet::from_space_separated(
            "GL_EXT_texture_rg GL_EXT_unpack_subimage GL_OES_element_index_uint \
             GL_OES_vertex_array_object GL_OES_EGL_image GL_OES_EGL_image_external \
             GL_KHR_debug GL_OES_texture_3D GL_EXT_robustness",
        ),
        egl_version: EglVersion { major: 1, minor: 5 },
        egl_vendor: "Mesa Project".to_owned(),
        egl_client_extensions: ExtensionSet::from_space_separated(
            "EGL_KHR_platform_wayland EGL_KHR_platform_x11 EGL_MESA_platform_surfaceless",
        ),
        egl_display_extensions: ExtensionSet::from_space_separated(
            "EGL_EXT_image_dma_buf_import EGL_EXT_image_dma_buf_import_modifiers \
             EGL_KHR_fence_sync EGL_ANDROID_native_fence_sync EGL_EXT_buffer_age \
             EGL_KHR_swap_buffers_with_damage EGL_EXT_create_context_robustness \
             EGL_KHR_surfaceless_context",
        ),
        max_texture_size: 8192,
        fragment_high_float_precision_bits: 23,
        dma_buf: DmaBufImportSupport::Queried {
            formats: vec![
                DmaBufFormatSupport {
                    fourcc: DrmFourcc::R8,
                    modifiers: vec![
                        sampler(DRM_FORMAT_MOD_LINEAR),
                        sampler(I915_FORMAT_MOD_X_TILED),
                    ],
                },
                DmaBufFormatSupport {
                    fourcc: DrmFourcc::GR88,
                    modifiers: vec![
                        sampler(DRM_FORMAT_MOD_LINEAR),
                        sampler(I915_FORMAT_MOD_X_TILED),
                    ],
                },
                DmaBufFormatSupport {
                    fourcc: DrmFourcc::ABGR8888,
                    modifiers: vec![
                        sampler(DRM_FORMAT_MOD_LINEAR),
                        sampler(I915_FORMAT_MOD_X_TILED),
                    ],
                },
                DmaBufFormatSupport {
                    fourcc: DRM_FORMAT_NV12,
                    modifiers: vec![external(DRM_FORMAT_MOD_LINEAR)],
                },
            ],
        },
    }
}

/// Radeon 780M (radeonsi): на запрос ES 2 — контекст ES 3.2, тайловые модификаторы, P010 через
/// GR88 + ABGR8888.
fn radeonsi_report() -> DriverReport {
    let mut report = ironlake_report();
    report.gl_version_string = "OpenGL ES 3.2 Mesa 26.2.5".to_owned();
    report.reported_gles_version = GlesVersion { major: 3, minor: 2 };
    report.gl_renderer = "AMD Radeon 780M Graphics (radeonsi, phoenix, ACO)".to_owned();
    report.max_texture_size = 16384;
    report.dma_buf = DmaBufImportSupport::Queried {
        formats: [DrmFourcc::R8, DrmFourcc::GR88, DrmFourcc::ABGR8888]
            .into_iter()
            .map(|fourcc| DmaBufFormatSupport {
                fourcc,
                modifiers: vec![sampler(AMD_TILED_MODIFIER), sampler(DRM_FORMAT_MOD_LINEAR)],
            })
            .collect(),
    };
    report
}

#[test]
fn ironlake_matches_the_session_00_table() {
    let capabilities = Gles2Capabilities::from_report(ironlake_report());
    let tiers = capabilities.tiers();
    assert_eq!(tiers.baseline, TierStatus::Available);
    assert_eq!(tiers.precision, TierStatus::Available);
    assert_eq!(tiers.zero_copy, TierStatus::Available);
    // Драйвер умеет импорт плоскостей P010 (GR88 + ABGR8888) и highp; что у Gen5 нет 10-битного
    // декода — забота декодера, а не возможностей рендера.
    assert_eq!(tiers.zero_copy_10bit, TierStatus::Available);
    assert_eq!(
        tiers.conveniences,
        Conveniences {
            unpack_subimage: true,
            element_index_uint: true,
            vertex_array_object: true,
            egl_fence_sync: true,
            buffer_age: true,
        }
    );
    assert_eq!(capabilities.max_texture_size(), 8192);
    assert_eq!(
        capabilities.dma_buf().sampling_of(DRM_FORMAT_NV12),
        Some(FormatSampling::ExternalOnly)
    );
}

#[test]
fn missing_abgr8888_removes_only_the_10bit_tier() {
    let mut report = ironlake_report();
    if let DmaBufImportSupport::Queried { formats } = &mut report.dma_buf {
        formats.retain(|format| format.fourcc != DrmFourcc::ABGR8888);
    }
    let tiers = Gles2Capabilities::from_report(report).tiers().clone();
    assert!(tiers.zero_copy.is_available());
    assert_eq!(
        tiers.zero_copy_10bit,
        TierStatus::Missing(vec![MissingRequirement::DmaBufFormat {
            fourcc: DrmFourcc::ABGR8888,
            sampling: None
        }])
    );
}

#[test]
fn radeonsi_has_all_five_tiers() {
    let capabilities = Gles2Capabilities::from_report(radeonsi_report());
    let tiers = capabilities.tiers();
    assert!(tiers.baseline.is_available());
    assert!(tiers.precision.is_available());
    assert!(tiers.zero_copy.is_available());
    assert!(tiers.zero_copy_10bit.is_available());
}

#[test]
fn es3_context_gets_no_es3_features_for_free() {
    // ES 3.2 умеет UNPACK_ROW_LENGTH и 32-битные индексы в ядре, но мы смотрим только на
    // расширения ES 2.0 — иначе на T410 (ES 2.0) будет чёрный экран.
    let mut report = radeonsi_report();
    report.gl_extensions = ExtensionSet::from_space_separated("GL_OES_EGL_image");
    let tiers = Gles2Capabilities::from_report(report).tiers().clone();
    assert!(!tiers.conveniences.unpack_subimage);
    assert!(!tiers.conveniences.element_index_uint);
    assert!(!tiers.conveniences.vertex_array_object);
}

#[test]
fn missing_highp_removes_precision_and_10bit_tiers() {
    let mut report = radeonsi_report();
    report.fragment_high_float_precision_bits = 0;
    let tiers = Gles2Capabilities::from_report(report).tiers().clone();
    assert_eq!(
        tiers.precision,
        TierStatus::Missing(vec![MissingRequirement::HighPrecisionFragmentFloat])
    );
    assert_eq!(
        tiers.zero_copy_10bit,
        TierStatus::Missing(vec![MissingRequirement::HighPrecisionFragmentFloat])
    );
    assert!(
        tiers.zero_copy.is_available(),
        "8-bit zero-copy does not need highp"
    );
}

#[test]
fn zero_copy_requires_modifiers_and_sampler2d_formats() {
    let mut without_modifiers = ironlake_report();
    without_modifiers.egl_display_extensions =
        ExtensionSet::from_space_separated("EGL_EXT_image_dma_buf_import EGL_KHR_fence_sync");
    without_modifiers.dma_buf = DmaBufImportSupport::WithoutFormatQuery;
    let tiers = Gles2Capabilities::from_report(without_modifiers)
        .tiers()
        .clone();
    assert_eq!(
        tiers.zero_copy,
        TierStatus::Missing(vec![
            MissingRequirement::EglExtension("EGL_EXT_image_dma_buf_import_modifiers"),
            MissingRequirement::DmaBufFormat {
                fourcc: DrmFourcc::R8,
                sampling: None
            },
            MissingRequirement::DmaBufFormat {
                fourcc: DrmFourcc::GR88,
                sampling: None
            },
        ])
    );

    let mut external_only = ironlake_report();
    external_only.dma_buf = DmaBufImportSupport::Queried {
        formats: vec![
            DmaBufFormatSupport {
                fourcc: DrmFourcc::R8,
                modifiers: vec![external(DRM_FORMAT_MOD_LINEAR)],
            },
            DmaBufFormatSupport {
                fourcc: DrmFourcc::GR88,
                modifiers: Vec::new(),
            },
        ],
    };
    let tiers = Gles2Capabilities::from_report(external_only)
        .tiers()
        .clone();
    assert_eq!(
        tiers.zero_copy,
        TierStatus::Missing(vec![
            MissingRequirement::DmaBufFormat {
                fourcc: DrmFourcc::R8,
                sampling: Some(FormatSampling::ExternalOnly)
            },
            MissingRequirement::DmaBufFormat {
                fourcc: DrmFourcc::GR88,
                sampling: Some(FormatSampling::ImplicitModifierOnly)
            },
        ])
    );
}

#[test]
fn no_fence_extension_means_no_fence_convenience() {
    let mut report = ironlake_report();
    report.egl_display_extensions = ExtensionSet::from_space_separated("EGL_EXT_buffer_age");
    let conveniences = Gles2Capabilities::from_report(report).tiers().conveniences;
    assert!(!conveniences.egl_fence_sync);
    assert!(conveniences.buffer_age);
}

#[test]
fn extension_lookup_matches_whole_names_only() {
    let extensions =
        ExtensionSet::from_space_separated("GL_EXT_texture_rg_extra  GL_OES_EGL_image");
    assert!(!extensions.contains("GL_EXT_texture_rg"));
    assert!(extensions.contains("GL_OES_EGL_image"));
    assert!(!extensions.contains("GL_OES_EGL"));
    assert_eq!(extensions.len(), 2);
}

#[test]
fn fourcc_codes_match_drm_fourcc_h() {
    // drm_fourcc.h: fourcc_code('R', '8', ' ', ' ') = 0x20203852 и т.д.
    assert_eq!(DrmFourcc::R8.0, 0x2020_3852);
    assert_eq!(DrmFourcc::GR88.0, 0x3838_5247);
    assert_eq!(DrmFourcc::ABGR8888.0, 0x3432_4241);
    assert_eq!(format!("{:?}", DrmFourcc::GR88), "DrmFourcc(\"GR88\")");
}

/// Открытый fake-display: аренда держит его инициализированным до конца теста.
struct FakeDisplay {
    fake: std::sync::Arc<FakeEgl>,
    lease: DisplayLease,
    _registry: std::sync::Arc<DisplayRegistry>,
}

impl FakeDisplay {
    fn id(&self) -> DisplayId {
        self.lease.display()
    }
}

fn fake_display(behavior: FakeBehavior) -> FakeDisplay {
    let (fake, registry) = FakeEgl::with_registry(behavior);
    let lease = match registry.acquire(NativeDisplay::Surfaceless) {
        Ok(lease) => lease,
        Err(error) => panic!("fake display must open: {error}"),
    };
    FakeDisplay {
        fake,
        lease,
        _registry: registry,
    }
}

#[test]
fn dma_buf_query_collects_formats_and_modifiers() {
    let display = fake_display(FakeBehavior {
        dma_buf_formats: Ok(vec![
            (DrmFourcc::R8.0, vec![sampler(DRM_FORMAT_MOD_LINEAR)]),
            (DRM_FORMAT_NV12.0, vec![external(DRM_FORMAT_MOD_LINEAR)]),
        ]),
        ..FakeBehavior::default()
    });
    let extensions = ExtensionSet::from_space_separated(
        "EGL_EXT_image_dma_buf_import EGL_EXT_image_dma_buf_import_modifiers",
    );
    let support = dma_buf_support(display.fake.as_ref(), display.id(), &extensions);
    assert_eq!(
        support.sampling_of(DrmFourcc::R8),
        Some(FormatSampling::Sampler2D)
    );
    assert_eq!(
        support.sampling_of(DRM_FORMAT_NV12),
        Some(FormatSampling::ExternalOnly)
    );
    assert_eq!(support.sampling_of(DrmFourcc::GR88), None);
}

#[test]
fn dma_buf_query_failure_and_missing_extensions_are_distinct() {
    let display = fake_display(FakeBehavior {
        dma_buf_formats: Err(EglErrorCode::BadAlloc),
        ..FakeBehavior::default()
    });
    let full = ExtensionSet::from_space_separated(
        "EGL_EXT_image_dma_buf_import EGL_EXT_image_dma_buf_import_modifiers",
    );
    assert!(matches!(
        dma_buf_support(display.fake.as_ref(), display.id(), &full),
        DmaBufImportSupport::QueryFailed { .. }
    ));
    assert_eq!(
        dma_buf_support(
            display.fake.as_ref(),
            display.id(),
            &ExtensionSet::from_space_separated("EGL_EXT_image_dma_buf_import")
        ),
        DmaBufImportSupport::WithoutFormatQuery
    );
    assert_eq!(
        dma_buf_support(
            display.fake.as_ref(),
            display.id(),
            &ExtensionSet::default()
        ),
        DmaBufImportSupport::Unavailable
    );
}
