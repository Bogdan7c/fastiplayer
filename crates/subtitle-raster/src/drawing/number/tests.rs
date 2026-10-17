use super::parse_number;

/// Разбирает строку с начала и возвращает значение и число прочитанных байт.
fn parse(text: &str) -> Option<(f64, usize)> {
    parse_number(text.as_bytes(), 0)
}

#[test]
fn plain_integers_and_decimals_are_read() {
    assert_eq!(parse("10"), Some((10.0, 2)));
    assert_eq!(parse("-1.5"), Some((-1.5, 4)));
    assert_eq!(parse("+3"), Some((3.0, 2)));
    assert_eq!(parse("0"), Some((0.0, 1)));
    assert_eq!(parse("000"), Some((0.0, 3)));
}

#[test]
fn leading_or_trailing_point_is_a_number() {
    assert_eq!(parse(".5"), Some((0.5, 2)));
    assert_eq!(parse("5."), Some((5.0, 2)));
    let (value, end) = parse("0.05").expect("number");
    assert!((value - 0.05).abs() < 1e-15, "{value}");
    assert_eq!(end, 4);
}

#[test]
fn input_without_digits_is_not_a_number() {
    assert_eq!(parse(""), None);
    assert_eq!(parse("abc"), None);
    assert_eq!(parse("-"), None);
    assert_eq!(parse("."), None);
    assert_eq!(parse("-."), None);
    assert_eq!(parse("e5"), None);
}

#[test]
fn leading_whitespace_is_skipped_and_end_points_after_the_number() {
    assert_eq!(parse("  \t7 8"), Some((7.0, 4)));
    // Следующее число читается с позиции за предыдущим.
    let text = b"7 8";
    let (first, end) = parse_number(text, 0).expect("first");
    let (second, end) = parse_number(text, end).expect("second");
    assert_eq!((first, second, end), (7.0, 8.0, 3));
}

#[test]
fn exponent_is_applied_and_a_dangling_exponent_letter_is_consumed() {
    assert_eq!(parse("1e2"), Some((100.0, 3)));
    let (value, end) = parse("1E-2").expect("number");
    assert!((value - 0.01).abs() < 1e-17, "{value}");
    assert_eq!(end, 4);
    assert_eq!(parse("2.5e+1"), Some((25.0, 6)));
    // libass съедает `e` и знак даже без цифр порядка (C: `p += 1` до цикла по цифрам).
    assert_eq!(parse("5e"), Some((5.0, 2)));
    assert_eq!(parse("5e-"), Some((5.0, 3)));
}

#[test]
fn huge_exponents_saturate_instead_of_wrapping() {
    let (value, end) = parse("1e999").expect("number");
    assert!(value.is_infinite() && value > 0.0);
    assert_eq!(end, 5);
    let (small, _) = parse("1e-999").expect("number");
    assert_eq!(small, 0.0);
}

#[test]
fn mantissa_longer_than_eighteen_digits_keeps_the_leading_ones() {
    // 22 цифры: значащими остаются первые 18, остальные только сдвигают порядок.
    let (value, end) = parse("1234567890123456789012").expect("number");
    assert_eq!(end, 22);
    // Эталон через стандартный разбор: литерал с 18 значащими цифрами clippy считает избыточным.
    let expected: f64 = "1.23456789012345678e21".parse().expect("reference");
    assert!((value / expected - 1.0).abs() < 1e-12, "{value}");
}

#[test]
fn typical_drawing_coordinates_match_the_standard_parser() {
    for text in ["100", "0.25", "-37.5", "12.75", "3.14159", "1000000"] {
        let (value, end) = parse(text).expect("number");
        let expected: f64 = text.parse().expect("std parse");
        assert!((value - expected).abs() <= expected.abs() * 1e-14, "{text}");
        assert_eq!(end, text.len());
    }
}
