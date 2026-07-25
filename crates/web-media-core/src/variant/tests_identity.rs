//! Opaque identity, redaction и refresh-stability proofs.

use super::test_support::*;
use super::*;

#[test]
fn semantic_identity_survives_parent_reextraction_while_exact_identity_changes() {
    let old_parent = parent_at_generation(1, 7, "old-parent", "stable-parent");
    let fresh_parent = parent_at_generation(1, 8, "fresh-parent", "stable-parent");
    assert_ne!(old_parent.exact(), fresh_parent.exact());
    assert_eq!(old_parent.semantic(), fresh_parent.semantic());

    let old_catalog = catalog_identity(old_parent, 3);
    let fresh_catalog = catalog_identity(fresh_parent, 1);
    let old_exact = ComponentVariantExactIdentity::new(
        old_catalog.clone(),
        ComponentKind::Video,
        ComponentVariantExactKey::new("old-exact").expect("old exact key должен быть valid"),
    );
    let fresh_exact = ComponentVariantExactIdentity::new(
        fresh_catalog.clone(),
        ComponentKind::Video,
        ComponentVariantExactKey::new("fresh-exact").expect("fresh exact key должен быть valid"),
    );
    assert_ne!(old_exact, fresh_exact);

    let old_semantic = ComponentVariantSemanticIdentity::new(
        old_catalog.parent().semantic().clone(),
        ComponentKind::Video,
        ComponentVariantSemanticKey::new("stable-variant")
            .expect("old semantic key должен быть valid"),
    );
    let fresh_semantic = ComponentVariantSemanticIdentity::new(
        fresh_catalog.parent().semantic().clone(),
        ComponentKind::Video,
        ComponentVariantSemanticKey::new("stable-variant")
            .expect("fresh semantic key должен быть valid"),
    );
    assert_eq!(old_semantic, fresh_semantic);

    let rematched_variant =
        VideoComponentVariant::new(fresh_exact, old_semantic, video_track(Some(720)));
    assert!(
        ComponentVariantCatalog::new(
            fresh_catalog,
            generous_limit(),
            ComponentVariantCatalogEntries::VideoOnly {
                video: vec![rematched_variant],
            },
        )
        .is_ok()
    );
}

#[test]
fn keys_reject_empty_control_and_byte_overflow_without_debug_disclosure() {
    assert_eq!(
        ComponentVariantExactKey::new(""),
        Err(ComponentVariantKeyError::Empty)
    );
    assert_eq!(
        ComponentVariantSemanticKey::new("line\nbreak"),
        Err(ComponentVariantKeyError::ContainsControlCharacter)
    );
    let exact_bound = "я".repeat(MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES / 2);
    assert!(ComponentVariantExactKey::new(exact_bound).is_ok());
    let oversized = "я".repeat(MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES / 2 + 1);
    assert_eq!(
        ComponentVariantSemanticKey::new(oversized),
        Err(ComponentVariantKeyError::TooLong {
            provided_bytes: MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES + 2,
            maximum_bytes: MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES,
        })
    );

    let secret = "signed-provider-variant-secret";
    let exact = ComponentVariantExactKey::new(secret).expect("key должен быть valid");
    let semantic = ComponentVariantSemanticKey::new(secret).expect("key должен быть valid");
    assert!(!format!("{exact:?}").contains(secret));
    assert!(!format!("{semantic:?}").contains(secret));
}
