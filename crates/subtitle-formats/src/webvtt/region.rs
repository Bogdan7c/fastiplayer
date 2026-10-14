//! Блок `REGION`: именованный прямоугольник, в который складываются реплики.
//!
//! Настройки: `id`, `width`, `lines`, `regionanchor`, `viewportanchor`, `scroll`.
//! Некорректная настройка отбрасывается по отдельности (остальные сохраняются);
//! регион без `id` отбрасывается целиком, потому что на него нельзя сослаться.

use subtitle_core::{RegionPoint, RegionScroll, SubtitleRegion};

use super::cue_settings::parse_percent;
use super::timestamp::is_webvtt_space;

/// Итог разбора одного блока `REGION`.
#[derive(Debug, PartialEq)]
pub(super) struct ParsedRegionBlock {
    /// `None`, если у региона нет непустого `id`.
    pub(super) region: Option<SubtitleRegion>,
    /// Сколько настроек пришлось отбросить как некорректные.
    pub(super) rejected_setting_count: usize,
}

/// Разбирает строки блока после заголовка `REGION`.
pub(super) fn parse_region_block<'text>(
    setting_lines: impl IntoIterator<Item = &'text str>,
) -> ParsedRegionBlock {
    let mut region = SubtitleRegion::with_webvtt_defaults(String::new());
    let mut rejected_setting_count = 0;
    for token in setting_lines
        .into_iter()
        .flat_map(|line| line.split(is_webvtt_space))
        .filter(|token| !token.is_empty())
    {
        let applied = token
            .split_once(':')
            .filter(|(name, value)| !name.is_empty() && !value.is_empty())
            .and_then(|(name, value)| apply_setting(&mut region, name, value));
        if applied.is_none() {
            rejected_setting_count += 1;
        }
    }
    ParsedRegionBlock {
        region: (!region.id.is_empty()).then_some(region),
        rejected_setting_count,
    }
}

/// Добавляет регион в список. Если регион с таким `id` уже есть, прежний удаляется:
/// по W3C побеждает более поздний.
pub(super) fn register_region(regions: &mut Vec<SubtitleRegion>, region: SubtitleRegion) {
    regions.retain(|existing| existing.id != region.id);
    regions.push(region);
}

/// Применяет одну настройку; `None` — настройка некорректна и ничего не изменила.
fn apply_setting(region: &mut SubtitleRegion, name: &str, value: &str) -> Option<()> {
    match name {
        "id" => {
            if value.contains("-->") {
                return None;
            }
            region.id = value.to_owned();
        }
        "width" => region.width = parse_percent(value)?,
        "lines" => region.lines = parse_positive_lines(value)?,
        "regionanchor" => region.region_anchor = parse_point(value)?,
        "viewportanchor" => region.viewport_anchor = parse_point(value)?,
        "scroll" => {
            region.scroll = match value {
                "up" => RegionScroll::Up,
                _ => return None,
            }
        }
        _ => return None,
    }
    Some(())
}

/// Количество строк: целое не меньше 1 (регион в ноль строк бессмыслен).
fn parse_positive_lines(value: &str) -> Option<u32> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|lines| *lines > 0)
}

/// `x%,y%`.
fn parse_point(value: &str) -> Option<RegionPoint> {
    let (x_text, y_text) = value.split_once(',')?;
    Some(RegionPoint {
        x: parse_percent(x_text)?,
        y: parse_percent(y_text)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use subtitle_core::PercentOfVideo;

    fn percent(value: f32) -> PercentOfVideo {
        PercentOfVideo::new(value).unwrap()
    }

    #[test]
    fn all_region_settings_are_parsed_across_lines() {
        let parsed = parse_region_block([
            "id:fred width:40%",
            "lines:3 regionanchor:0%,100%",
            "viewportanchor:10%,90% scroll:up",
        ]);
        let region = parsed.region.unwrap();
        assert_eq!(region.id, "fred");
        assert_eq!(region.width, percent(40.0));
        assert_eq!(region.lines, 3);
        assert_eq!(region.region_anchor.x, percent(0.0));
        assert_eq!(region.region_anchor.y, percent(100.0));
        assert_eq!(region.viewport_anchor.x, percent(10.0));
        assert_eq!(region.viewport_anchor.y, percent(90.0));
        assert_eq!(region.scroll, RegionScroll::Up);
        assert_eq!(parsed.rejected_setting_count, 0);
    }

    #[test]
    fn omitted_settings_keep_webvtt_defaults() {
        let region = parse_region_block(["id:r"]).region.unwrap();
        assert_eq!(region, SubtitleRegion::with_webvtt_defaults("r"));
    }

    #[test]
    fn invalid_settings_are_counted_and_do_not_disturb_valid_ones() {
        let parsed = parse_region_block([
            "id:r width:150% lines:0 lines:x scroll:down regionanchor:5% viewportanchor:1%,2 foo:1 lonely width:25%",
        ]);
        let region = parsed.region.unwrap();
        assert_eq!(region.width, percent(25.0));
        assert_eq!(region.lines, 3);
        assert_eq!(region.scroll, RegionScroll::None);
        assert_eq!(parsed.rejected_setting_count, 8);
    }

    #[test]
    fn region_without_id_is_dropped() {
        assert_eq!(parse_region_block(["width:50%"]).region, None);
        assert_eq!(parse_region_block(["id:--> width:50%"]).region, None);
    }

    #[test]
    fn later_region_with_same_id_replaces_earlier_one() {
        let mut regions = Vec::new();
        let first = parse_region_block(["id:a width:10%"]).region.unwrap();
        let other = parse_region_block(["id:b"]).region.unwrap();
        let second = parse_region_block(["id:a width:20%"]).region.unwrap();
        register_region(&mut regions, first);
        register_region(&mut regions, other);
        register_region(&mut regions, second);
        let ids: Vec<&str> = regions.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert_eq!(regions[1].width, percent(20.0));
    }
}
