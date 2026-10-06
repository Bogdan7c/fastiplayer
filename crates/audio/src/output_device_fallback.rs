//! Порядок попыток открыть устройство вывода: выбранное → системное по умолчанию.
//!
//! Политика владельца (UX edge cases, сессия 10): пропавшее устройство не должно выключать
//! звук целиком — звук уходит на устройство по умолчанию, а пользователь получает
//! уведомление. Выбор в настройках при этом **не меняется**: запасной маршрут действует
//! только для одного созданного output-а, следующее открытие снова пробует выбранное.
//!
//! Модуль не знает CPAL: открытие устройства передаётся замыканием, поэтому порядок попыток
//! и формирование маршрута проверяются тестами без звуковой карты.

use anyhow::Result;
use audio_core::{AudioOutputDeviceRequest, AudioOutputDeviceRoute};
use tracing::warn;

use crate::devices::{DEFAULT_AUDIO_OUTPUT_DEVICE_ID, output_device_display_name};

/// Открывает устройство согласно запросу player-а и сообщает фактический маршрут.
///
/// - `selected_device_id` — stable id из настроек (только читается);
/// - `open_device` — открывает output на stable id (в production — CPAL).
///
/// Если запасное устройство тоже не открылось, ошибка содержит обе причины.
pub(crate) fn open_output_for_device_request<OpenedOutput>(
    selected_device_id: &str,
    device_request: AudioOutputDeviceRequest,
    mut open_device: impl FnMut(&str) -> Result<OpenedOutput>,
) -> Result<(OpenedOutput, AudioOutputDeviceRoute)> {
    match device_request {
        AudioOutputDeviceRequest::SelectedOnly => open_device(selected_device_id)
            .map(|output| (output, AudioOutputDeviceRoute::SelectedDevice)),
        AudioOutputDeviceRequest::SystemDefault => open_device(DEFAULT_AUDIO_OUTPUT_DEVICE_ID)
            .map(|output| (output, AudioOutputDeviceRoute::SystemDefault)),
        AudioOutputDeviceRequest::SelectedOrSystemDefault => {
            open_selected_or_system_default(selected_device_id, open_device)
        }
    }
}

/// Пробует выбранное устройство, при неудаче — системное по умолчанию.
fn open_selected_or_system_default<OpenedOutput>(
    selected_device_id: &str,
    mut open_device: impl FnMut(&str) -> Result<OpenedOutput>,
) -> Result<(OpenedOutput, AudioOutputDeviceRoute)> {
    let selected_error = match open_device(selected_device_id) {
        Ok(output) => return Ok((output, AudioOutputDeviceRoute::SelectedDevice)),
        Err(error) => error,
    };

    // Выбрано и так «по умолчанию»: запасного варианта нет, повторять ту же попытку бессмысленно.
    if selected_device_id == DEFAULT_AUDIO_OUTPUT_DEVICE_ID {
        return Err(selected_error);
    }

    warn!(
        error = %selected_error,
        "Выбранное устройство вывода недоступно; пробуем устройство по умолчанию"
    );
    match open_device(DEFAULT_AUDIO_OUTPUT_DEVICE_ID) {
        Ok(output) => Ok((
            output,
            AudioOutputDeviceRoute::SystemDefaultInsteadOfUnavailable {
                unavailable_device_name: output_device_display_name(selected_device_id),
            },
        )),
        Err(default_error) => Err(default_error.context(format!(
            "запасное устройство по умолчанию тоже недоступно (выбранное: {selected_error:#})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    /// Stable id «наушников» в формате CPAL 0.15, как его сохраняют настройки.
    const HEADSET_ID: &str = "cpal-0.15-name:front%3ACARD%3DHeadset%2CDEV%3D0";

    /// Fake «звуковая система»: открываются только перечисленные id, журнал попыток сохраняется.
    struct FakeDevices {
        available_ids: Vec<&'static str>,
        attempted_ids: Vec<String>,
    }

    impl FakeDevices {
        fn with_available(available_ids: Vec<&'static str>) -> Self {
            Self {
                available_ids,
                attempted_ids: Vec::new(),
            }
        }

        /// Возвращает «output» = id открытого устройства, чтобы тест видел, что открыто.
        fn open(&mut self, device_id: &str) -> Result<String> {
            self.attempted_ids.push(device_id.to_string());
            if self.available_ids.contains(&device_id) {
                Ok(device_id.to_string())
            } else {
                Err(anyhow!("device {device_id} is unavailable"))
            }
        }
    }

    #[test]
    fn available_selected_device_is_used_without_touching_default() {
        let mut devices = FakeDevices::with_available(vec![HEADSET_ID, "default"]);

        let (opened, route) = open_output_for_device_request(
            HEADSET_ID,
            AudioOutputDeviceRequest::SelectedOrSystemDefault,
            |id| devices.open(id),
        )
        .expect("выбранное устройство доступно");

        assert_eq!(opened, HEADSET_ID);
        assert_eq!(route, AudioOutputDeviceRoute::SelectedDevice);
        assert_eq!(devices.attempted_ids, vec![HEADSET_ID]);
    }

    #[test]
    fn missing_selected_device_falls_back_to_default_with_its_name() {
        let mut devices = FakeDevices::with_available(vec!["default"]);

        let (opened, route) = open_output_for_device_request(
            HEADSET_ID,
            AudioOutputDeviceRequest::SelectedOrSystemDefault,
            |id| devices.open(id),
        )
        .expect("default доступен");

        assert_eq!(opened, "default");
        assert_eq!(
            route,
            AudioOutputDeviceRoute::SystemDefaultInsteadOfUnavailable {
                unavailable_device_name: "front:CARD=Headset,DEV=0".to_string(),
            }
        );
        assert_eq!(devices.attempted_ids, vec![HEADSET_ID, "default"]);
    }

    #[test]
    fn no_devices_at_all_reports_both_causes() {
        let mut devices = FakeDevices::with_available(Vec::new());

        let error = open_output_for_device_request(
            HEADSET_ID,
            AudioOutputDeviceRequest::SelectedOrSystemDefault,
            |id| devices.open(id),
        )
        .expect_err("ни одного устройства нет");

        let message = format!("{error:#}");
        assert!(
            message.contains("device default is unavailable"),
            "{message}"
        );
        assert!(message.contains(HEADSET_ID), "{message}");
    }

    #[test]
    fn selected_default_that_fails_is_not_retried() {
        let mut devices = FakeDevices::with_available(Vec::new());

        let error = open_output_for_device_request(
            "default",
            AudioOutputDeviceRequest::SelectedOrSystemDefault,
            |id| devices.open(id),
        )
        .expect_err("default недоступен");

        assert!(format!("{error:#}").contains("device default is unavailable"));
        assert_eq!(devices.attempted_ids, vec!["default"]);
    }

    #[test]
    fn selected_only_request_never_substitutes_default() {
        let mut devices = FakeDevices::with_available(vec!["default"]);

        let error = open_output_for_device_request(
            HEADSET_ID,
            AudioOutputDeviceRequest::SelectedOnly,
            |id| devices.open(id),
        )
        .expect_err("явный выбор пользователя нельзя подменять");

        assert!(format!("{error:#}").contains(HEADSET_ID));
        assert_eq!(devices.attempted_ids, vec![HEADSET_ID]);
    }

    #[test]
    fn system_default_request_skips_broken_selected_device() {
        let mut devices = FakeDevices::with_available(vec![HEADSET_ID, "default"]);

        let (opened, route) = open_output_for_device_request(
            HEADSET_ID,
            AudioOutputDeviceRequest::SystemDefault,
            |id| devices.open(id),
        )
        .expect("default доступен");

        assert_eq!(opened, "default");
        assert_eq!(route, AudioOutputDeviceRoute::SystemDefault);
        assert_eq!(devices.attempted_ids, vec!["default"]);
    }
}
