//! Импорт плоскости DMA-BUF на fake-драйвере EGL и fake-стороне GL: без GPU.
//!
//! Проверяется результат: какие атрибуты ушли драйверу, что `EGLImage` уничтожен в любом исходе,
//! что счётчики честны, что fd не закрыт, и что отказ ДО драйвера драйвер не трогает.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, AsRawFd};

use super::attach::{ImageEgl, ImportCapabilities, TextureImageTarget, attach_plane_to_texture};
use super::*;
use crate::capabilities::{DmaBufFormatSupport, DmaBufImportSupport, ExtensionSet};
use crate::context::driver::{DmaBufModifier, EglErrorCode, EglOperation, NativeDisplay};
use crate::test_support::fake_egl::{FakeBehavior, FakeCall, FakeEgl};

/// Модификатор AMD из живого вывода разведки: LO = 0x10401b04, HI = 0x02000000.
const AMD_MODIFIER: DrmModifier = DrmModifier(0x0200_0000_1040_1b04);
const GL_INVALID_OPERATION: u32 = 0x0502;

/// Вызов GL, записанный fake-стороной.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlCall {
    Bind(Option<u32>),
    Target(ImageId),
    TakeError,
}

struct FakeGl {
    function_loaded: bool,
    calls: RefCell<Vec<GlCall>>,
    /// Флаги ошибок, ждущие снятия `glGetError`.
    error_flags: RefCell<VecDeque<u32>>,
    /// Ошибка, которую оставит `glEGLImageTargetTexture2DOES`.
    target_error: Cell<u32>,
}

impl FakeGl {
    fn new() -> Self {
        Self {
            function_loaded: true,
            calls: RefCell::new(Vec::new()),
            error_flags: RefCell::new(VecDeque::new()),
            target_error: Cell::new(0),
        }
    }
}

impl TextureImageTarget for FakeGl {
    fn image_target_function_loaded(&self) -> bool {
        self.function_loaded
    }

    fn bind_texture_2d(&self, texture: Option<glow::NativeTexture>) {
        self.calls
            .borrow_mut()
            .push(GlCall::Bind(texture.map(|texture| texture.0.get())));
    }

    fn target_bound_texture_from_image(&self, image: ImageId) {
        self.calls.borrow_mut().push(GlCall::Target(image));
        if self.target_error.get() != 0 {
            self.error_flags
                .borrow_mut()
                .push_back(self.target_error.get());
        }
    }

    fn take_error_flag(&self) -> u32 {
        self.calls.borrow_mut().push(GlCall::TakeError);
        self.error_flags.borrow_mut().pop_front().unwrap_or(0)
    }
}

/// Всё, что нужно одному сценарию.
struct Rig {
    fake: std::sync::Arc<FakeEgl>,
    lease: crate::context::display_registry::DisplayLease,
    gl: FakeGl,
    gl_extensions: ExtensionSet,
    egl_extensions: ExtensionSet,
    dma_buf: DmaBufImportSupport,
    stats: Cell<DmaBufTextureStats>,
    dev_null: std::fs::File,
}

fn sampler(modifier: DrmModifier) -> DmaBufModifier {
    DmaBufModifier {
        modifier: modifier.0,
        external_only: false,
    }
}

fn rig_with(behavior: FakeBehavior) -> Rig {
    let (fake, registry) = FakeEgl::with_registry(behavior);
    let lease = registry
        .acquire(NativeDisplay::Surfaceless)
        .expect("fake display opens");
    Rig {
        fake,
        lease,
        gl: FakeGl::new(),
        gl_extensions: ExtensionSet::from_space_separated("GL_OES_EGL_image"),
        egl_extensions: ExtensionSet::from_space_separated(
            "EGL_EXT_image_dma_buf_import EGL_EXT_image_dma_buf_import_modifiers",
        ),
        dma_buf: DmaBufImportSupport::Queried {
            formats: vec![DmaBufFormatSupport {
                fourcc: DrmFourcc::R8,
                modifiers: vec![sampler(DrmModifier::LINEAR), sampler(AMD_MODIFIER)],
            }],
        },
        stats: Cell::new(DmaBufTextureStats::default()),
        dev_null: std::fs::File::open("/dev/null").expect("open /dev/null"),
    }
}

fn rig() -> Rig {
    rig_with(FakeBehavior::default())
}

impl Rig {
    fn plane(&self, modifier: DrmModifier) -> DmaBufPlaneTexture<'_> {
        DmaBufPlaneTexture {
            fourcc: DrmFourcc::R8,
            width: 1920,
            height: 1080,
            fd: self.dev_null.as_fd(),
            offset: 4096,
            pitch: 2048,
            modifier,
        }
    }

    fn attach(
        &self,
        plane: &DmaBufPlaneTexture<'_>,
    ) -> Result<DmaBufTextureAttached, DmaBufTextureError> {
        let texture = glow::NativeTexture(NonZeroU32::new(7).expect("non-zero"));
        attach_plane_to_texture(
            &ImageEgl {
                driver: self.lease.driver(),
                display: self.lease.display(),
            },
            &ImportCapabilities {
                gl_extensions: &self.gl_extensions,
                egl_extensions: &self.egl_extensions,
                dma_buf: &self.dma_buf,
            },
            &self.gl,
            texture,
            plane,
            &self.stats,
        )
    }

    fn image_calls(&self) -> usize {
        self.fake
            .count(|call| matches!(call, FakeCall::CreateImage(_) | FakeCall::DestroyImage(_)))
    }
}

/// Как раздуть одно поле плоскости сверх `EGLint`.
type Overflow = fn(&mut DmaBufPlaneTexture<'_>, u32);

fn expect_err(outcome: Result<DmaBufTextureAttached, DmaBufTextureError>) -> DmaBufTextureError {
    outcome.expect_err("attach must fail")
}

#[test]
fn attributes_put_every_field_in_place_and_split_modifier_into_lo_hi() {
    let rig = rig();
    let plane = rig.plane(AMD_MODIFIER);
    let attributes = DmaBufImageAttributes::build(&plane).expect("fits EGLint");
    let raw_fd = usize::try_from(rig.dev_null.as_raw_fd()).expect("fd >= 0");
    let expected = vec![
        0x3057,
        1920, // EGL_WIDTH
        0x3056,
        1080, // EGL_HEIGHT
        0x3271,
        0x2020_3852, // EGL_LINUX_DRM_FOURCC_EXT = 'R8  '
        0x3272,
        raw_fd, // PLANE0_FD
        0x3273,
        4096, // PLANE0_OFFSET
        0x3274,
        2048, // PLANE0_PITCH
        0x3443,
        0x1040_1b04, // MODIFIER_LO
        0x3444,
        0x0200_0000, // MODIFIER_HI
        0x3038,      // EGL_NONE
    ];
    assert_eq!(attributes.as_slice(), expected.as_slice());
    assert_eq!(
        attributes.modifier(),
        ModifierAttribute::Explicit(AMD_MODIFIER)
    );
}

#[test]
fn invalid_modifier_sends_no_modifier_attributes_and_ends_with_none() {
    let rig = rig();
    let plane = rig.plane(DrmModifier::INVALID);
    let attributes = DmaBufImageAttributes::build(&plane).expect("fits EGLint");
    let list = attributes.as_slice();
    assert!(!list.contains(&0x3443) && !list.contains(&0x3444));
    assert_eq!(list.last(), Some(&0x3038));
    assert_eq!(list.len(), 13, "6 пар + EGL_NONE");
    assert_eq!(attributes.modifier(), ModifierAttribute::DriverChosen);
}

#[test]
fn linear_modifier_is_sent_explicitly_not_confused_with_invalid() {
    let rig = rig();
    let plane = rig.plane(DrmModifier::LINEAR);
    let attributes = DmaBufImageAttributes::build(&plane).expect("fits EGLint");
    let list = attributes.as_slice();
    assert_eq!(&list[12..16], &[0x3443, 0, 0x3444, 0]);
    assert_eq!(
        attributes.modifier(),
        ModifierAttribute::Explicit(DrmModifier::LINEAR)
    );
}

#[test]
fn values_above_egl_int_are_rejected_before_the_driver() {
    let too_big = u32::try_from(i32::MAX).expect("positive") + 1;
    let cases: [(DmaBufAttribute, Overflow); 4] = [
        (DmaBufAttribute::Width, |plane, value| plane.width = value),
        (DmaBufAttribute::Height, |plane, value| plane.height = value),
        (DmaBufAttribute::Offset, |plane, value| plane.offset = value),
        (DmaBufAttribute::Pitch, |plane, value| plane.pitch = value),
    ];
    for (attribute, overflow) in cases {
        let rig = rig();
        let mut plane = rig.plane(DrmModifier::LINEAR);
        overflow(&mut plane, too_big);

        let error = expect_err(rig.attach(&plane));

        assert_eq!(error, DmaBufTextureError::AttributeOutOfRange { attribute });
        assert_eq!(
            rig.image_calls(),
            0,
            "{attribute:?}: driver must not be called"
        );
        assert!(rig.fake.image_attribute_lists().is_empty());
        assert!(
            rig.gl.calls.borrow().is_empty(),
            "{attribute:?}: GL untouched"
        );
        assert_eq!(rig.stats.get().attach_failures, 1);
    }
}

#[test]
fn i32_max_still_fits() {
    let rig = rig();
    let mut plane = rig.plane(DrmModifier::LINEAR);
    plane.offset = u32::try_from(i32::MAX).expect("positive");
    rig.attach(&plane).expect("i32::MAX is a legal EGLint");
}

#[test]
fn successful_attach_creates_binds_then_destroys_and_counts_honestly() {
    let rig = rig();
    let plane = rig.plane(AMD_MODIFIER);

    let attached = rig.attach(&plane).expect("attach succeeds");

    assert_eq!(attached.modifier, ModifierAttribute::Explicit(AMD_MODIFIER));
    assert_eq!(attached.image_release, ImageRelease::Released);
    let journal = rig.fake.journal();
    let created = journal
        .iter()
        .find_map(|call| match call {
            FakeCall::CreateImage(image) => Some(*image),
            _ => None,
        })
        .expect("image created");
    assert!(journal.contains(&FakeCall::DestroyImage(created)));
    // GL: bind → (снятие старых флагов) → target → проверка ошибки → отвязка.
    let calls = rig.gl.calls.borrow();
    assert_eq!(calls.first(), Some(&GlCall::Bind(Some(7))));
    assert_eq!(calls.last(), Some(&GlCall::Bind(None)));
    assert!(calls.contains(&GlCall::Target(created)));
    assert_eq!(rig.fake.live_image_count(), 0);
    assert_eq!(
        rig.stats.get(),
        DmaBufTextureStats {
            images_created: 1,
            images_destroyed: 1,
            image_destroy_failures: 0,
            attach_failures: 0,
            attaches_succeeded: 1,
        }
    );
}

#[test]
fn driver_refusal_reports_error_never_destroys_and_leaves_gl_alone() {
    let rig = rig_with(FakeBehavior {
        image_create_error: Some(EglErrorCode::BadMatch),
        ..FakeBehavior::default()
    });

    let error = expect_err(rig.attach(&rig.plane(AMD_MODIFIER)));

    assert_eq!(
        error,
        DmaBufTextureError::ImageCreation(crate::context::driver::EglCallError::new(
            EglOperation::CreateImage,
            EglErrorCode::BadMatch
        ))
    );
    assert_eq!(
        rig.fake
            .count(|call| matches!(call, FakeCall::DestroyImage(_))),
        0
    );
    assert!(rig.gl.calls.borrow().is_empty(), "GL не должна трогаться");
    let stats = rig.stats.get();
    assert_eq!((stats.images_created, stats.images_destroyed), (0, 0));
    assert_eq!(stats.attach_failures, 1);
}

#[test]
fn missing_modifiers_extension_blocks_explicit_modifier_without_calling_driver() {
    let mut rig = rig();
    rig.egl_extensions = ExtensionSet::from_space_separated("EGL_EXT_image_dma_buf_import");

    let error = expect_err(rig.attach(&rig.plane(AMD_MODIFIER)));

    assert_eq!(
        error,
        DmaBufTextureError::ZeroCopyUnsupported {
            missing: DmaBufMissingRequirement::EglExtImageDmaBufImportModifiers
        }
    );
    assert_eq!(rig.image_calls(), 0);
    assert!(rig.fake.image_attribute_lists().is_empty());

    // INVALID модификатор не требует `_modifiers`: с ним та же среда импортирует.
    rig.dma_buf = DmaBufImportSupport::WithoutFormatQuery;
    rig.attach(&rig.plane(DrmModifier::INVALID))
        .expect("invalid modifier needs only the base extension");
}

#[test]
fn missing_base_extensions_are_named() {
    let mut rig = rig();
    rig.gl_extensions = ExtensionSet::default();
    assert_eq!(
        expect_err(rig.attach(&rig.plane(DrmModifier::LINEAR))),
        DmaBufTextureError::ZeroCopyUnsupported {
            missing: DmaBufMissingRequirement::GlOesEglImage
        }
    );
    rig.gl_extensions = ExtensionSet::from_space_separated("GL_OES_EGL_image");
    rig.egl_extensions = ExtensionSet::default();
    assert_eq!(
        expect_err(rig.attach(&rig.plane(DrmModifier::LINEAR))),
        DmaBufTextureError::ZeroCopyUnsupported {
            missing: DmaBufMissingRequirement::EglExtImageDmaBufImport
        }
    );
    assert_eq!(rig.image_calls(), 0);
}

#[test]
fn missing_target_function_is_typed_error_before_the_driver() {
    let mut rig = rig();
    rig.gl.function_loaded = false;
    assert_eq!(
        expect_err(rig.attach(&rig.plane(DrmModifier::LINEAR))),
        DmaBufTextureError::ImageTargetFunctionMissing
    );
    assert_eq!(rig.image_calls(), 0);
}

#[test]
fn external_only_pair_is_not_sampleable_and_driver_stays_untouched() {
    let mut rig = rig();
    rig.dma_buf = DmaBufImportSupport::Queried {
        formats: vec![DmaBufFormatSupport {
            fourcc: DrmFourcc::R8,
            modifiers: vec![DmaBufModifier {
                modifier: AMD_MODIFIER.0,
                external_only: true,
            }],
        }],
    };

    let error = expect_err(rig.attach(&rig.plane(AMD_MODIFIER)));

    assert_eq!(
        error,
        DmaBufTextureError::FormatModifierNotSampleable {
            fourcc: DrmFourcc::R8,
            modifier: AMD_MODIFIER,
            sampling: ModifierSampling::ExternalOnly,
        }
    );
    assert_eq!(rig.image_calls(), 0);
    // Модификатор, которого драйвер не называл, — тоже отказ.
    let error = expect_err(rig.attach(&rig.plane(DrmModifier::LINEAR)));
    assert!(matches!(
        error,
        DmaBufTextureError::FormatModifierNotSampleable {
            sampling: ModifierSampling::NotListed,
            ..
        }
    ));
}

#[test]
fn gl_error_from_target_is_reported_and_image_is_still_destroyed() {
    let rig = rig();
    rig.gl.target_error.set(GL_INVALID_OPERATION);

    let error = expect_err(rig.attach(&rig.plane(AMD_MODIFIER)));

    assert_eq!(
        error,
        DmaBufTextureError::TextureTarget {
            gl_error: GL_INVALID_OPERATION
        }
    );
    assert_eq!(rig.fake.live_image_count(), 0, "образ не должен утечь");
    let stats = rig.stats.get();
    assert_eq!((stats.images_created, stats.images_destroyed), (1, 1));
    assert_eq!((stats.attach_failures, stats.attaches_succeeded), (1, 0));
    assert_eq!(
        rig.gl.calls.borrow().last(),
        Some(&GlCall::Bind(None)),
        "текстура отвязана и при ошибке"
    );
}

#[test]
fn stale_gl_error_flags_are_not_blamed_on_the_import() {
    let rig = rig();
    rig.gl
        .error_flags
        .borrow_mut()
        .push_back(GL_INVALID_OPERATION);

    rig.attach(&rig.plane(AMD_MODIFIER))
        .expect("старый флаг ошибки не наш");
}

#[test]
fn destroy_failure_after_successful_target_is_reported_not_lost() {
    let rig = rig_with(FakeBehavior {
        image_destroy_error: Some(EglErrorCode::BadAlloc),
        ..FakeBehavior::default()
    });

    let attached = rig
        .attach(&rig.plane(AMD_MODIFIER))
        .expect("texture is valid");

    assert!(matches!(
        attached.image_release,
        ImageRelease::DestroyFailed(error) if error.code == EglErrorCode::BadAlloc
    ));
    let stats = rig.stats.get();
    assert_eq!(stats.images_created, 1);
    assert_eq!(
        (stats.images_destroyed, stats.image_destroy_failures),
        (0, 1)
    );
    assert_eq!(stats.attaches_succeeded, 1);
}

#[test]
fn api_borrows_fd_and_never_closes_it() {
    let rig = rig();
    let raw_fd = rig.dev_null.as_raw_fd();
    let proc_path = format!("/proc/self/fd/{raw_fd}");

    rig.attach(&rig.plane(AMD_MODIFIER)).expect("success path");
    assert!(
        std::path::Path::new(&proc_path).exists(),
        "fd жив после успеха"
    );

    let broken = rig_with(FakeBehavior {
        image_create_error: Some(EglErrorCode::BadAlloc),
        ..FakeBehavior::default()
    });
    let raw_fd = broken.dev_null.as_raw_fd();
    assert!(broken.attach(&broken.plane(AMD_MODIFIER)).is_err());
    assert!(
        std::path::Path::new(&format!("/proc/self/fd/{raw_fd}")).exists(),
        "fd жив после ошибки"
    );
}

#[test]
fn lost_context_rejects_before_touching_the_driver() {
    // Настоящий offscreen-контекст нужен только как носитель состояния здоровья.
    let Some(size) = crate::SurfaceSize::new(4, 4) else {
        panic!("4×4 is a valid size");
    };
    let context = match Gles2Context::offscreen(size) {
        Ok(context) => context,
        Err(error) => {
            assert!(
                std::env::var_os("GLES2_EGUI_REQUIRE_EGL").is_none(),
                "EGL is required but unavailable: {error}"
            );
            eprintln!("skip lost_context_rejects_before_touching_the_driver: {error}");
            return;
        }
    };
    let rig = rig();
    context.simulate_context_loss(crate::loss::ContextLoss::from_egl());
    let texture = glow::NativeTexture(NonZeroU32::new(1).expect("non-zero"));

    let error = context
        .attach_dma_buf_plane_to_texture(texture, &rig.plane(DrmModifier::LINEAR))
        .expect_err("lost context cannot import");

    assert!(matches!(
        error,
        DmaBufTextureError::ContextUnavailable(GlAccessError::ContextLost(_))
    ));
    let stats = context.dma_buf_texture_stats();
    assert_eq!((stats.images_created, stats.attach_failures), (0, 1));
}

#[test]
fn sampling_of_modifier_follows_driver_listing() {
    let external = DmaBufModifier {
        modifier: 5,
        external_only: true,
    };
    let support = DmaBufImportSupport::Queried {
        formats: vec![
            DmaBufFormatSupport {
                fourcc: DrmFourcc::R8,
                modifiers: vec![sampler(DrmModifier::LINEAR), external],
            },
            DmaBufFormatSupport {
                fourcc: DrmFourcc::GR88,
                modifiers: vec![external],
            },
            DmaBufFormatSupport {
                fourcc: DrmFourcc::ABGR8888,
                modifiers: vec![],
            },
        ],
    };
    let sampling = |fourcc, modifier| support.sampling_of_modifier(fourcc, modifier);
    assert_eq!(
        sampling(DrmFourcc::R8, DrmModifier::LINEAR),
        ModifierSampling::Sampler2D
    );
    assert_eq!(
        sampling(DrmFourcc::R8, DrmModifier(5)),
        ModifierSampling::ExternalOnly
    );
    assert_eq!(
        sampling(DrmFourcc::R8, DrmModifier(9)),
        ModifierSampling::NotListed
    );
    assert_eq!(
        sampling(DrmFourcc::R8, DrmModifier::INVALID),
        ModifierSampling::Sampler2D
    );
    assert_eq!(
        sampling(DrmFourcc::GR88, DrmModifier::INVALID),
        ModifierSampling::ExternalOnly
    );
    // Пустой список модификаторов + INVALID: годится (реализация выберет сама).
    assert_eq!(
        sampling(DrmFourcc::ABGR8888, DrmModifier::INVALID),
        ModifierSampling::Sampler2D
    );
    assert_eq!(
        sampling(DrmFourcc::ABGR8888, DrmModifier::LINEAR),
        ModifierSampling::NotListed
    );
    assert_eq!(
        sampling(DrmFourcc(1), DrmModifier::INVALID),
        ModifierSampling::NotListed
    );
    assert_eq!(
        DmaBufImportSupport::WithoutFormatQuery
            .sampling_of_modifier(DrmFourcc::R8, DrmModifier::LINEAR),
        ModifierSampling::FormatQueryUnavailable
    );
    assert_eq!(
        DmaBufImportSupport::Unavailable.sampling_of_modifier(DrmFourcc::R8, DrmModifier::LINEAR),
        ModifierSampling::ImportUnavailable
    );
}
