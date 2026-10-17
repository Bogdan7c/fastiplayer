use super::*;

/// Токены текста как пары `(вид, x, y)` в целых пикселях (координаты 26.6 делятся на 64).
fn tokens_of(text: &str) -> Vec<(TokenKind, i32, i32)> {
    tokenize(text.as_bytes())
        .into_iter()
        .map(|token| (token.kind, token.point.x / 64, token.point.y / 64))
        .collect()
}

#[test]
fn move_and_line_commands_become_one_token_per_point() {
    assert_eq!(
        tokens_of("m 0 0 l 10 0 10 10"),
        [
            (TokenKind::Move, 0, 0),
            (TokenKind::Line, 10, 0),
            (TokenKind::Line, 10, 10),
        ]
    );
}

#[test]
fn coordinates_are_converted_to_26_dot_6_with_rounding() {
    let tokens = tokenize(b"m 0.5 -1.25");
    assert_eq!(tokens.len(), 1);
    assert_eq!((tokens[0].point.x, tokens[0].point.y), (32, -80));
}

#[test]
fn unknown_characters_are_ignored_and_commands_before_the_root_are_dropped() {
    assert_eq!(
        tokens_of("xyz l 1 1 b 1 1 2 2 3 3 m 3 4 q"),
        [(TokenKind::Move, 3, 4)]
    );
}

#[test]
fn n_before_any_m_rejects_the_whole_drawing() {
    assert!(tokenize(b"n 1 1").is_empty());
    assert!(tokenize(b"l 5 5 n 1 1 m 0 0 l 1 1").is_empty());
}

#[test]
fn n_after_a_broken_m_becomes_the_root() {
    assert_eq!(
        tokens_of("m x n 1 1 l 2 2"),
        [
            (TokenKind::MoveWithoutClosing, 1, 1),
            (TokenKind::Line, 2, 2),
        ]
    );
}

#[test]
fn several_point_pairs_after_m_are_all_moves() {
    assert_eq!(
        tokens_of("m 0 0 5 5 l 6 6"),
        [
            (TokenKind::Move, 0, 0),
            (TokenKind::Move, 5, 5),
            (TokenKind::Line, 6, 6),
        ]
    );
}

#[test]
fn incomplete_trailing_points_are_consumed_but_not_added() {
    assert_eq!(tokens_of("m 0 0 l 1"), [(TokenKind::Move, 0, 0)]);
    assert_eq!(
        tokens_of("m 0 0 l 1 1 2"),
        [(TokenKind::Move, 0, 0), (TokenKind::Line, 1, 1)]
    );
    // Пять точек Безье: берутся три, хвост из двух отбрасывается.
    assert_eq!(
        tokens_of("m 0 0 b 1 1 2 2 3 3 4 4 5 5"),
        [
            (TokenKind::Move, 0, 0),
            (TokenKind::CubicBezier, 1, 1),
            (TokenKind::CubicBezier, 2, 2),
            (TokenKind::CubicBezier, 3, 3),
        ]
    );
}

#[test]
fn spline_has_three_start_points_then_extensions_and_c_closes_it() {
    assert_eq!(
        tokens_of("m 0 0 s 1 1 2 2 3 3 4 4 c"),
        [
            (TokenKind::Move, 0, 0),
            (TokenKind::BSpline, 1, 1),
            (TokenKind::BSpline, 2, 2),
            (TokenKind::BSpline, 3, 3),
            (TokenKind::ExtendSpline, 4, 4),
            // c повторяет узел перед сплайном и две его первые точки.
            (TokenKind::ExtendSpline, 0, 0),
            (TokenKind::ExtendSpline, 1, 1),
            (TokenKind::ExtendSpline, 2, 2),
        ]
    );
}

#[test]
fn p_needs_three_nodes_and_c_needs_a_spline() {
    assert_eq!(tokens_of("m 0 0 p 1 1"), [(TokenKind::Move, 0, 0)]);
    assert_eq!(tokens_of("m 0 0 c"), [(TokenKind::Move, 0, 0)]);
    assert_eq!(
        tokens_of("m 0 0 l 1 1 2 2 p 3 3"),
        [
            (TokenKind::Move, 0, 0),
            (TokenKind::Line, 1, 1),
            (TokenKind::Line, 2, 2),
            (TokenKind::ExtendSpline, 3, 3),
        ]
    );
}

#[test]
fn a_spline_with_fewer_than_three_points_is_dropped() {
    assert_eq!(tokens_of("m 0 0 s 1 1 2 2 c"), [(TokenKind::Move, 0, 0)]);
}

#[test]
fn text_stops_at_the_first_nul() {
    assert_eq!(tokens_of("m 1 1\0 l 2 2"), [(TokenKind::Move, 1, 1)]);
}
