use skrifa::FontRef;
use subtitle_core::SubtitleColor;

use super::*;
use crate::event_style::{AxisPair, BorderStyle, StyleColors};
use crate::fonts::FontWeightClass;

const NOTO_REGULAR: &[u8] = include_bytes!("../../../fonts/NotoSans-Regular.ttf");
const NOTO_BOLD: &[u8] = include_bytes!("../../../fonts/NotoSans-Bold.ttf");
const NOTO_ARMENIAN: &[u8] = include_bytes!("../../../fonts/NotoSansArmenian-Regular.ttf");

fn style_of(bytes: &'static [u8]) -> FaceStyleInfo {
    FaceStyleInfo::from_font(&FontRef::new(bytes).expect("font"))
}

/// Вид символа с запрошенным весом и наклоном; остальное нейтрально.
fn look(weight_value: u16, slant: FontStyle, decorations: TextDecorations) -> SpanLook {
    let white = SubtitleColor::opaque(255, 255, 255);
    SpanLook {
        font_family: None,
        font_size: 40.0,
        weight: FontWeightClass::from_os2_weight(weight_value),
        weight_value,
        slant,
        colors: StyleColors {
            primary: white,
            secondary: white,
            outline: white,
            back: white,
        },
        border: AxisPair::uniform(0.0),
        shadow: AxisPair::uniform(0.0),
        border_style: BorderStyle::OutlineAndShadow,
        scale: AxisPair::uniform(1.0),
        letter_spacing: 0.0,
        decorations,
        rotation: crate::glyph_transform::Rotation3d::default(),
        shear: crate::glyph_transform::ShearFactors::default(),
    }
}

#[test]
fn regular_face_reports_weight_flags_and_decoration_lines() {
    let regular = style_of(NOTO_REGULAR);
    assert_eq!(regular.weight, 400);
    assert!(!regular.is_bold && !regular.is_italic && !regular.is_postscript());
    let underline = regular.underline().expect("Noto has a post underline");
    assert!(underline.position_units <= 0 && underline.thickness_units > 0);
    let strike = regular.strike_out().expect("Noto has an OS/2 strikeout");
    assert!(strike.position_units >= 0 && strike.thickness_units > 0);
}

#[test]
fn bold_face_is_marked_bold_with_weight_700() {
    let bold = style_of(NOTO_BOLD);
    assert!(bold.is_bold);
    assert_eq!(bold.weight, 700);
}

#[test]
fn italic_is_synthesized_only_when_the_face_lacks_it() {
    let regular = style_of(NOTO_REGULAR);
    let plain = TextDecorations::default();
    assert_eq!(
        regular
            .variant_for(&look(400, FontStyle::Italic, plain))
            .synthetic_italic,
        SyntheticItalic::Needed,
        "Noto Sans Regular is upright, italic request needs the shear"
    );
    assert_eq!(
        regular
            .variant_for(&look(400, FontStyle::Normal, plain))
            .synthetic_italic,
        SyntheticItalic::NotNeeded
    );
}

#[test]
fn bold_is_synthesized_past_the_weight_margin_on_faces_without_bold() {
    let plain = TextDecorations::default();
    let armenian = style_of(NOTO_ARMENIAN);
    let needs = |weight: u16| {
        armenian
            .variant_for(&look(weight, FontStyle::Normal, plain))
            .synthetic_bold
    };
    assert_eq!(needs(400), SyntheticBold::NotNeeded);
    assert_eq!(
        needs(550),
        SyntheticBold::NotNeeded,
        "400 + 150 is still fine"
    );
    assert_eq!(needs(551), SyntheticBold::Needed);
    assert_eq!(needs(700), SyntheticBold::Needed);
    // У лица с флагом Bold утолщать нечего, как бы ни был велик запрошенный вес.
    let bold = style_of(NOTO_BOLD);
    assert_eq!(
        bold.variant_for(&look(900, FontStyle::Normal, plain))
            .synthetic_bold,
        SyntheticBold::NotNeeded
    );
}

#[test]
fn variant_carries_the_requested_decorations() {
    let decorations = TextDecorations {
        underline: true,
        strike_out: false,
    };
    let variant = style_of(NOTO_REGULAR).variant_for(&look(400, FontStyle::Normal, decorations));
    assert_eq!(variant.decorations, decorations);
    assert_ne!(variant, GlyphVariant::PLAIN);
}

#[test]
fn weight_class_table_follows_libass() {
    assert_eq!(face_weight(None, false), 400);
    assert_eq!(face_weight(None, true), 700);
    assert_eq!(face_weight(Some(0), true), 700);
    assert_eq!(face_weight(Some(1), false), 100);
    assert_eq!(face_weight(Some(4), false), 350);
    assert_eq!(face_weight(Some(6), false), 600);
    assert_eq!(face_weight(Some(9), false), 900);
    assert_eq!(face_weight(Some(450), false), 450);
}
