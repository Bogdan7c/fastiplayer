//! Тесты жизненного цикла вложенных шрифтов в библиотеке: подготовка (тяжёлая фаза),
//! установка (дешёвая фаза) и снятие. Проверяют наблюдаемое состояние библиотеки.

use std::sync::Arc;

use super::attached::{
    AttachedFontError, AttachedFontLimits, AttachedFontStatus, AttachedFontsClearOutcome,
    PreparedAttachedFont, PreparedAttachedFonts,
};
use super::face::FaceOrigin;
use super::library::SubtitleFontLibrary;
use super::lookup_cache::FontLookupCacheLimits;
use super::resolve::FamilyMatch;
use super::style::{FontRequest, FontStyle, FontWeightClass};
use super::test_fonts::{noto_bold_bytes, noto_regular_bytes, rename_family};

const LIMITS: AttachedFontLimits = AttachedFontLimits {
    max_font_bytes: 16 * 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
    max_faces: 64,
};

fn library() -> SubtitleFontLibrary {
    SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 64,
    })
    .expect("embedded fonts")
}

/// Копия Noto Sans под именем семейства `Noto Sanz`: по имени её не спутать со встроенной.
fn renamed_noto() -> Vec<u8> {
    let mut bytes = noto_regular_bytes();
    rename_family(&mut bytes, "Noto Sans", "Noto Sanz");
    bytes
}

fn request_for(family: &'static str) -> FontRequest<'static> {
    FontRequest {
        family: Some(family),
        weight: FontWeightClass::Regular,
        style: FontStyle::Normal,
    }
}

#[test]
fn prepared_values_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PreparedAttachedFont>();
    assert_send_sync::<PreparedAttachedFonts>();
}

#[test]
fn prepare_does_not_touch_any_library_and_install_makes_the_family_resolvable() {
    let mut fonts = library();
    let prepared =
        PreparedAttachedFont::prepare(Arc::from(renamed_noto()), &LIMITS).expect("valid font");
    assert_eq!(prepared.face_count(), 1);
    // Пока шрифт не установлен, имя не находится.
    assert_eq!(
        fonts.resolve_family(&request_for("Noto Sanz")).family_match,
        FamilyMatch::DefaultFallback
    );
    assert_eq!(fonts.statistics().attached_faces, 0);

    let size = prepared.byte_len();
    let registration = fonts
        .install_prepared_attached_font(prepared, &LIMITS)
        .expect("install");
    assert_eq!(registration.status, AttachedFontStatus::Registered);
    let resolved = fonts.resolve_family(&request_for("Noto Sanz"));
    assert_eq!(resolved.family_match, FamilyMatch::Requested);
    assert_eq!(resolved.origin, FaceOrigin::Attached);
    assert_eq!(fonts.statistics().attached_font_bytes, size);
}

#[test]
fn prepared_font_crosses_threads_before_install() {
    let bytes: Arc<[u8]> = Arc::from(renamed_noto());
    let prepared = std::thread::spawn(move || PreparedAttachedFont::prepare(bytes, &LIMITS))
        .join()
        .expect("worker thread")
        .expect("valid font");
    let mut fonts = library();
    fonts
        .install_prepared_attached_font(prepared, &LIMITS)
        .expect("install");
    assert_eq!(fonts.statistics().attached_faces, 1);
}

#[test]
fn prepare_errors_leave_the_library_unchanged() {
    let fonts = library();
    let before = fonts.statistics();
    let tiny_limits = AttachedFontLimits {
        max_font_bytes: 100,
        ..LIMITS
    };
    let too_large = PreparedAttachedFont::prepare(Arc::from(renamed_noto()), &tiny_limits)
        .expect_err("over the per-file limit");
    assert!(matches!(
        too_large,
        AttachedFontError::TooLarge { max_bytes: 100, .. }
    ));
    let garbage = PreparedAttachedFont::prepare(Arc::from(vec![7_u8; 2048]), &LIMITS)
        .expect_err("not a font");
    assert_eq!(garbage, AttachedFontError::Unparseable);
    assert_eq!(fonts.statistics(), before);
}

#[test]
fn batch_prepare_reports_rejections_by_input_index_and_keeps_good_files() {
    let files: Vec<Arc<[u8]>> = vec![
        Arc::from(vec![1_u8; 64]),
        Arc::from(renamed_noto()),
        Arc::from(noto_bold_bytes()),
    ];
    let tiny_limits = AttachedFontLimits {
        max_font_bytes: renamed_noto().len() + 1,
        ..LIMITS
    };
    let prepared = PreparedAttachedFonts::prepare(files, &tiny_limits);
    // Первый файл — мусор, третий (Bold) больше лимита: остаётся один.
    let rejected: Vec<(usize, bool)> = prepared
        .rejected()
        .iter()
        .map(|item| {
            (
                item.input_index,
                matches!(item.error, AttachedFontError::Unparseable),
            )
        })
        .collect();
    assert_eq!(prepared.prepared_count(), 1);
    assert_eq!(rejected.first(), Some(&(0, true)));
    let mut fonts = library();
    let report = fonts.install_prepared_attached_fonts(prepared, &tiny_limits);
    assert_eq!(report.registered, 1);
    assert_eq!(report.already_registered, 0);
    assert_eq!(report.rejected.len(), rejected.len());
}

#[test]
fn total_limits_are_enforced_across_prepare_and_install_and_reset_by_clear() {
    let first = renamed_noto();
    let mut second = noto_bold_bytes();
    rename_family(&mut second, "Noto Sans", "Noto Sanz");
    // Лимит на файл свободный, но суммарно помещается только больший из двух.
    let total_limit = first.len().max(second.len());
    let limits = AttachedFontLimits {
        max_total_bytes: total_limit,
        ..LIMITS
    };
    let prepared = PreparedAttachedFonts::prepare(
        [Arc::<[u8]>::from(first.clone()), Arc::from(second.clone())],
        &limits,
    );
    assert_eq!(prepared.prepared_count(), 2, "per-file limits pass");
    assert!(prepared.rejected().is_empty());

    let mut fonts = library();
    let report = fonts.install_prepared_attached_fonts(prepared, &limits);
    assert_eq!(report.registered, 1);
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.rejected[0].input_index, 1);
    assert!(matches!(
        report.rejected[0].error,
        AttachedFontError::TotalBytesExceeded { .. }
    ));
    assert_eq!(fonts.statistics().attached_faces, 1);

    // После снятия учёт обнуляется: второй файл теперь помещается.
    fonts.clear_attached_fonts();
    assert_eq!(fonts.statistics().attached_font_bytes, 0);
    let prepared_again = PreparedAttachedFont::prepare(Arc::from(second), &limits).expect("valid");
    fonts
        .install_prepared_attached_font(prepared_again, &limits)
        .expect("fits after clear");
}

#[test]
fn duplicate_install_is_reported_without_changing_the_library() {
    let mut fonts = library();
    let bytes = renamed_noto();
    let first = PreparedAttachedFont::prepare(Arc::from(bytes.clone()), &LIMITS).expect("valid");
    let registered = fonts
        .install_prepared_attached_font(first, &LIMITS)
        .expect("first");
    let again = PreparedAttachedFont::prepare(Arc::from(bytes), &LIMITS).expect("valid");
    let duplicate = fonts
        .install_prepared_attached_font(again, &LIMITS)
        .expect("duplicate");
    assert_eq!(duplicate.status, AttachedFontStatus::AlreadyRegistered);
    assert_eq!(duplicate.faces, registered.faces);
    assert_eq!(fonts.statistics().attached_faces, 1);
}

#[test]
fn clear_removes_only_attached_faces_and_reports_what_was_removed() {
    let mut fonts = library();
    assert_eq!(
        fonts.clear_attached_fonts(),
        AttachedFontsClearOutcome::NothingAttached
    );
    let bytes = renamed_noto();
    let size = bytes.len();
    let registration = fonts
        .register_attached_font(Arc::from(bytes), &LIMITS)
        .expect("valid");
    let attached_face = registration.faces[0];
    let embedded_before = fonts.statistics().embedded_faces;

    assert_eq!(
        fonts.clear_attached_fonts(),
        AttachedFontsClearOutcome::Cleared {
            faces: 1,
            bytes: size
        }
    );
    let after = fonts.statistics();
    assert_eq!(after.attached_faces, 0);
    assert_eq!(after.attached_font_bytes, 0);
    assert_eq!(after.embedded_faces, embedded_before);
    // Имя больше не находится, снятый FaceId мёртв и не переиспользуется.
    assert_eq!(
        fonts.resolve_family(&request_for("Noto Sanz")).family_match,
        FamilyMatch::DefaultFallback
    );
    assert!(fonts.describe_face(attached_face).is_none());
    assert!(fonts.face_bytes(attached_face).is_none());
    // Второй clear — снова «ничего».
    assert_eq!(
        fonts.clear_attached_fonts(),
        AttachedFontsClearOutcome::NothingAttached
    );

    // Новая регистрация получает НОВЫЙ FaceId.
    let again = fonts
        .register_attached_font(Arc::from(renamed_noto()), &LIMITS)
        .expect("valid");
    assert_eq!(again.status, AttachedFontStatus::Registered);
    assert_ne!(again.faces[0], attached_face);
}
