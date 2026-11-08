//! Предложение «Ask» режима 2 (сессия 12b) глазами зонда слоя экстеншена: разбор поля `viewer_offer`
//! (`ViewerOfferProbe` в `extensions/browser/src/shared/entry-contract.ts`). Прямоугольники — в
//! координатах окна СВОЕГО фрейма (как `return_button` заглушки), поэтому калибровка экрана та же.
//!
//! Отдельный модуль, чтобы разбор предложения не раздувал `extension_probe` и жил рядом со своим договором.

use serde_json::Value;

use super::extension_probe::{ButtonRect, parse_rect};

/// Кнопки панели предложения; `None` — кнопки не видно.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct OfferButtons {
    /// «Play in Fastiplayer».
    pub(super) fastiplayer: Option<ButtonRect>,
    /// «Play here».
    pub(super) here: Option<ButtonRect>,
    /// «Always for this site».
    pub(super) always: Option<ButtonRect>,
    /// «Never for this site».
    pub(super) never: Option<ButtonRect>,
}

/// Предложение «Ask» одного документа (или лучшего из фреймов вкладки).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct ViewerOfferState {
    /// Изолированный мир знает предложение документа.
    pub(super) offered: bool,
    /// Выбор за это предложение уже ушёл.
    pub(super) answered: bool,
    /// Панель на экране с ненулевым размером.
    pub(super) visible: bool,
    pub(super) panel: Option<ButtonRect>,
    /// `None` — панели не видно.
    pub(super) buttons: Option<OfferButtons>,
}

/// Разбор `viewer_offer` зонда. Битое поле — ошибка, а не «предложения нет»: иначе провал прятался бы.
pub(super) fn parse_viewer_offer(value: &Value) -> Result<ViewerOfferState, String> {
    let flag = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("viewer offer probe without {name}: {value}"))
    };
    let buttons = match value.get("buttons").filter(|buttons| !buttons.is_null()) {
        None => None,
        Some(buttons) => Some(OfferButtons {
            fastiplayer: parse_rect(buttons.get("fastiplayer"))?,
            here: parse_rect(buttons.get("here"))?,
            always: parse_rect(buttons.get("always"))?,
            never: parse_rect(buttons.get("never"))?,
        }),
    };
    Ok(ViewerOfferState {
        offered: flag("offered")?,
        answered: flag("answered")?,
        visible: flag("visible")?,
        panel: parse_rect(value.get("panel"))?,
        buttons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOWN: &str = r#"{"offered":true,"answered":false,"visible":true,"panel":{"left":10,"top":20,"width":300,"height":120},"buttons":{"fastiplayer":{"left":1,"top":2,"width":50,"height":20},"here":{"left":60,"top":2,"width":50,"height":20},"always":{"left":1,"top":30,"width":50,"height":20},"never":{"left":60,"top":30,"width":50,"height":20}}}"#;

    #[test]
    fn a_shown_offer_gives_the_panel_and_every_button_rect() {
        let offer = parse_viewer_offer(&serde_json::from_str(SHOWN).unwrap()).expect("parses");
        assert!(offer.offered && !offer.answered && offer.visible);
        assert_eq!(offer.panel.map(|panel| panel.width), Some(300.0));
        let buttons = offer.buttons.expect("buttons");
        assert_eq!(buttons.fastiplayer.map(|rect| rect.left), Some(1.0));
        assert_eq!(buttons.here.map(|rect| rect.left), Some(60.0));
        assert_eq!(buttons.always.map(|rect| rect.top), Some(30.0));
        assert_eq!(buttons.never.map(|rect| rect.top), Some(30.0));
    }

    #[test]
    fn an_idle_offer_has_no_panel_and_no_buttons() {
        let idle =
            r#"{"offered":false,"answered":false,"visible":false,"panel":null,"buttons":null}"#;
        let offer = parse_viewer_offer(&serde_json::from_str(idle).unwrap()).expect("parses");
        assert_eq!(offer, ViewerOfferState::default());
    }

    #[test]
    fn a_broken_offer_probe_is_an_error_not_an_absent_offer() {
        let missing_flag = serde_json::json!({"offered": true, "visible": true});
        assert!(parse_viewer_offer(&missing_flag).is_err());
        let broken_button = serde_json::json!({
            "offered": true, "answered": false, "visible": true, "panel": null,
            "buttons": {"fastiplayer": {"left": 1}}
        });
        assert!(parse_viewer_offer(&broken_button).is_err());
    }
}
