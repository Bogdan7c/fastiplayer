//! Сверка полного списка тегов `ass_parse_tags` (libass 0.17.5) с разбором: каждый тег
//! даёт ровно один разобранный тег ожидаемого вида, без «неизвестных» и «кривых». Это закрывает
//! обязательство плана «всё поддерживается или явно объяснено»: растеризатор обрабатывает
//! каждый вариант `AssOverride` исчерпывающим `match`, поэтому тег, который разбирается
//! здесь, не может молча потеряться при рисовании.

use subtitle_core::{AssOverride, AssTextItem};

use super::limits::AssParseLimits;
use super::report::AssTagStats;
use super::text::parse_event_text;

/// Разобранные теги блока `{<block>}x` вместе со счётчиками разбора.
fn parse_block(block: &str) -> (Vec<AssOverride>, usize, usize) {
    let limits = AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 16,
        max_override_tags_per_event: 1000,
        max_tracked_unknown_tag_names: 4,
    };
    let mut stats = AssTagStats::new(&limits);
    let items = parse_event_text(&format!("{{{block}}}x"), 1000, &mut stats);
    let tags = match items.into_iter().next() {
        Some(AssTextItem::Overrides(tags)) => tags,
        other => panic!("{block}: expected an override block, got {other:?}"),
    };
    (tags, stats.unknown_total(), stats.malformed())
}

/// Каждая строка: блок с одним тегом и проверка вида его разбора.
#[test]
fn every_tag_of_ass_parse_tags_is_parsed_into_its_own_override() {
    type Matcher = fn(&AssOverride) -> bool;
    let table: &[(&str, Matcher)] = &[
        (r"\xbord2", |tag| matches!(tag, AssOverride::Border { .. })),
        (r"\ybord2", |tag| matches!(tag, AssOverride::Border { .. })),
        (r"\xshad2", |tag| matches!(tag, AssOverride::Shadow { .. })),
        (r"\yshad2", |tag| matches!(tag, AssOverride::Shadow { .. })),
        (r"\fax0.2", |tag| matches!(tag, AssOverride::Shear { .. })),
        (r"\fay0.2", |tag| matches!(tag, AssOverride::Shear { .. })),
        (r"\iclip(1,2,30,40)", |tag| {
            matches!(tag, AssOverride::Clip(_))
        }),
        (r"\blur2", |tag| matches!(tag, AssOverride::Blur(_))),
        (r"\fscx110", |tag| matches!(tag, AssOverride::ScaleX(_))),
        (r"\fscy110", |tag| matches!(tag, AssOverride::ScaleY(_))),
        (r"\fsc", |tag| matches!(tag, AssOverride::ResetScale)),
        (r"\fsp2", |tag| matches!(tag, AssOverride::Spacing(_))),
        (r"\fs30", |tag| matches!(tag, AssOverride::FontSize(_))),
        (r"\bord2", |tag| matches!(tag, AssOverride::Border { .. })),
        (r"\move(1,2,3,4)", |tag| matches!(tag, AssOverride::Move(_))),
        (r"\frx10", |tag| matches!(tag, AssOverride::Rotation { .. })),
        (r"\fry10", |tag| matches!(tag, AssOverride::Rotation { .. })),
        (r"\frz10", |tag| matches!(tag, AssOverride::Rotation { .. })),
        (r"\fr10", |tag| matches!(tag, AssOverride::Rotation { .. })),
        (r"\fnArial", |tag| matches!(tag, AssOverride::FontName(_))),
        (r"\alpha&H80&", |tag| {
            matches!(tag, AssOverride::Alpha { .. })
        }),
        (r"\an5", |tag| matches!(tag, AssOverride::Alignment(_))),
        (r"\a5", |tag| matches!(tag, AssOverride::Alignment(_))),
        (r"\pos(1,2)", |tag| {
            matches!(tag, AssOverride::Position { .. })
        }),
        (r"\fade(0,255,0,0,100,200,300)", |tag| {
            matches!(tag, AssOverride::Fade(_))
        }),
        (r"\fad(100,200)", |tag| matches!(tag, AssOverride::Fade(_))),
        (r"\org(1,2)", |tag| {
            matches!(tag, AssOverride::Origin { .. })
        }),
        (r"\t(0,100,\fs50)", |tag| {
            matches!(tag, AssOverride::Transform(_))
        }),
        (r"\clip(1,2,30,40)", |tag| {
            matches!(tag, AssOverride::Clip(_))
        }),
        (r"\c&HFF&", |tag| matches!(tag, AssOverride::Color { .. })),
        (r"\1c&HFF&", |tag| matches!(tag, AssOverride::Color { .. })),
        (r"\2c&HFF&", |tag| matches!(tag, AssOverride::Color { .. })),
        (r"\3c&HFF&", |tag| matches!(tag, AssOverride::Color { .. })),
        (r"\4c&HFF&", |tag| matches!(tag, AssOverride::Color { .. })),
        (r"\1a&H80&", |tag| matches!(tag, AssOverride::Alpha { .. })),
        (r"\2a&H80&", |tag| matches!(tag, AssOverride::Alpha { .. })),
        (r"\3a&H80&", |tag| matches!(tag, AssOverride::Alpha { .. })),
        (r"\4a&H80&", |tag| matches!(tag, AssOverride::Alpha { .. })),
        (r"\r", |tag| matches!(tag, AssOverride::Reset(_))),
        (r"\be2", |tag| matches!(tag, AssOverride::EdgeBlur(_))),
        (r"\b1", |tag| matches!(tag, AssOverride::Bold(_))),
        (r"\i1", |tag| matches!(tag, AssOverride::Italic(_))),
        (r"\kt10", |tag| matches!(tag, AssOverride::KaraokeSkip(_))),
        (r"\kf10", |tag| matches!(tag, AssOverride::Karaoke(_))),
        (r"\K10", |tag| matches!(tag, AssOverride::Karaoke(_))),
        (r"\ko10", |tag| matches!(tag, AssOverride::Karaoke(_))),
        (r"\k10", |tag| matches!(tag, AssOverride::Karaoke(_))),
        (r"\shad2", |tag| matches!(tag, AssOverride::Shadow { .. })),
        (r"\s1", |tag| matches!(tag, AssOverride::StrikeOut(_))),
        (r"\u1", |tag| matches!(tag, AssOverride::Underline(_))),
        (r"\pbo5", |tag| {
            matches!(tag, AssOverride::DrawingBaselineOffset(_))
        }),
        (r"\p1", |tag| matches!(tag, AssOverride::Drawing(_))),
        (r"\q2", |tag| matches!(tag, AssOverride::WrapStyle(_))),
        (r"\fe0", |tag| matches!(tag, AssOverride::FontEncoding(_))),
    ];
    for (block, is_expected) in table {
        let (tags, unknown, malformed) = parse_block(block);
        assert_eq!((unknown, malformed), (0, 0), "{block}");
        assert_eq!(tags.len(), 1, "{block}: {tags:?}");
        assert!(is_expected(&tags[0]), "{block}: parsed as {:?}", tags[0]);
    }
}

#[test]
fn line_breaks_and_hard_space_become_text_items() {
    let items = parse_event_text(r"a\Nb\nc\hd", 1000, &mut stats());
    assert!(items.contains(&AssTextItem::HardBreak), "{items:?}");
    assert!(items.contains(&AssTextItem::SoftBreak), "{items:?}");
    let text: String = items
        .iter()
        .filter_map(|item| match item {
            AssTextItem::Text(content) => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "abc\u{a0}d", "`\\h` is a no-break space");
}

fn stats() -> AssTagStats {
    AssTagStats::new(&AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 16,
        max_override_tags_per_event: 1000,
        max_tracked_unknown_tag_names: 4,
    })
}
