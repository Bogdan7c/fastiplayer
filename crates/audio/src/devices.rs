//! Owner API для audio output devices поверх CPAL.
//!
//! Наружу выходят только stable string ids и display text. CPAL types остаются
//! внутри `audio`, поэтому settings runtime/UI не зависят от concrete backend-а.

use std::fmt;
use std::sync::{Arc, RwLock};

use cpal::traits::{DeviceTrait, HostTrait};

/// Зарезервированный stable id для системного output device по умолчанию.
pub const DEFAULT_AUDIO_OUTPUT_DEVICE_ID: &str = "default";

/// Префикс best-effort id для CPAL 0.15, где backend DeviceId ещё недоступен.
const CPAL_015_NAME_ID_PREFIX: &str = "cpal-0.15-name:";

/// Snapshot одного output device без CPAL types за пределами `audio`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputDeviceInfo {
    /// Stable id, который можно хранить в config.
    pub stable_id: String,

    /// User-facing имя устройства.
    pub display_name: String,

    /// Совпадает ли устройство с текущим system default по best-effort сравнению.
    pub is_system_default: bool,
}

/// Был ли выбранный output device реально изменён.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputDeviceSelectionChange {
    /// Новый id отличается от предыдущего.
    Applied,

    /// Выбранный id уже был активным.
    Noop,
}

/// Ошибка owner API audio output devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioOutputDeviceError {
    /// Shared selection state был poisoned.
    SelectionStateUnavailable,

    /// CPAL не смог перечислить output devices.
    EnumerationFailed { message: String },

    /// CPAL вернул device, но не смог дать имя, необходимое для CPAL 0.15 id.
    DeviceNameUnavailable { message: String },

    /// Stable id сейчас не соответствует ни одному output device.
    DeviceUnavailable { stable_id: String },

    /// Stable id пустой и не может быть применён.
    EmptyDeviceId,
}

impl fmt::Display for AudioOutputDeviceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelectionStateUnavailable => {
                formatter.write_str("audio output device selection state is unavailable")
            }
            Self::EnumerationFailed { message } => {
                write!(
                    formatter,
                    "audio output device enumeration failed: {message}"
                )
            }
            Self::DeviceNameUnavailable { message } => {
                write!(
                    formatter,
                    "audio output device name is unavailable: {message}"
                )
            }
            Self::DeviceUnavailable { stable_id } => {
                write!(
                    formatter,
                    "audio output device `{stable_id}` is unavailable"
                )
            }
            Self::EmptyDeviceId => formatter.write_str("audio output device id is empty"),
        }
    }
}

impl std::error::Error for AudioOutputDeviceError {}

/// Shared owner handle для выбранного audio output device.
///
/// Playback worker получает clone этого handle-а через `CpalAudioOutputFactory`.
/// Settings runtime меняет тот же handle при Apply, не раскрывая CPAL наружу.
#[derive(Debug, Clone)]
pub struct AudioOutputDeviceController {
    selected_device_id: Arc<RwLock<String>>,
}

impl Default for AudioOutputDeviceController {
    fn default() -> Self {
        Self::new(DEFAULT_AUDIO_OUTPUT_DEVICE_ID)
    }
}

impl AudioOutputDeviceController {
    /// Создаёт controller с уже сохранённым stable id.
    #[must_use]
    pub fn new(selected_device_id: impl Into<String>) -> Self {
        Self {
            selected_device_id: Arc::new(RwLock::new(selected_device_id.into())),
        }
    }

    /// Возвращает текущий stable id без доступа к CPAL.
    pub fn selected_device_id(&self) -> Result<String, AudioOutputDeviceError> {
        self.selected_device_id
            .read()
            .map(|selected_device_id| selected_device_id.clone())
            .map_err(|_| AudioOutputDeviceError::SelectionStateUnavailable)
    }

    /// Меняет выбранный stable id для следующих создаваемых CPAL outputs.
    pub fn select_output_device(
        &self,
        stable_id: impl Into<String>,
    ) -> Result<AudioOutputDeviceSelectionChange, AudioOutputDeviceError> {
        let stable_id = stable_id.into();
        if stable_id.trim().is_empty() {
            return Err(AudioOutputDeviceError::EmptyDeviceId);
        }

        let mut selected_device_id = self
            .selected_device_id
            .write()
            .map_err(|_| AudioOutputDeviceError::SelectionStateUnavailable)?;
        if *selected_device_id == stable_id {
            return Ok(AudioOutputDeviceSelectionChange::Noop);
        }

        *selected_device_id = stable_id;
        Ok(AudioOutputDeviceSelectionChange::Applied)
    }

    /// Перечисляет CPAL output devices как neutral snapshots.
    pub fn output_devices(&self) -> Result<Vec<AudioOutputDeviceInfo>, AudioOutputDeviceError> {
        list_output_devices()
    }
}

/// Перечисляет CPAL output devices без передачи CPAL types вызывающему коду.
pub fn list_output_devices() -> Result<Vec<AudioOutputDeviceInfo>, AudioOutputDeviceError> {
    let host = cpal::default_host();
    let default_output_name = host
        .default_output_device()
        .and_then(|device| device.name().ok());
    let devices =
        host.output_devices()
            .map_err(|error| AudioOutputDeviceError::EnumerationFailed {
                message: error.to_string(),
            })?;

    let mut infos = Vec::new();
    let mut seen_names = Vec::<String>::new();
    for device in devices {
        let display_name =
            device
                .name()
                .map_err(|error| AudioOutputDeviceError::DeviceNameUnavailable {
                    message: error.to_string(),
                })?;
        let duplicate_index = seen_names
            .iter()
            .filter(|seen_name| *seen_name == &display_name)
            .count();
        seen_names.push(display_name.clone());

        infos.push(AudioOutputDeviceInfo {
            stable_id: cpal_015_stable_id_from_name(&display_name, duplicate_index),
            is_system_default: default_output_name.as_ref() == Some(&display_name),
            display_name,
        });
    }

    Ok(infos)
}

/// Выбирает CPAL device по stable id, оставаясь внутри audio owner boundary.
pub(crate) fn output_device_from_stable_id(
    host: &cpal::Host,
    stable_id: &str,
) -> Result<cpal::Device, AudioOutputDeviceError> {
    if stable_id == DEFAULT_AUDIO_OUTPUT_DEVICE_ID {
        return host.default_output_device().ok_or_else(|| {
            AudioOutputDeviceError::DeviceUnavailable {
                stable_id: stable_id.to_string(),
            }
        });
    }

    let Some((requested_name, requested_duplicate_index)) = parse_cpal_015_name_id(stable_id)
    else {
        return Err(AudioOutputDeviceError::DeviceUnavailable {
            stable_id: stable_id.to_string(),
        });
    };

    let devices =
        host.output_devices()
            .map_err(|error| AudioOutputDeviceError::EnumerationFailed {
                message: error.to_string(),
            })?;
    let mut matched_duplicate_index = 0usize;
    for device in devices {
        let display_name =
            device
                .name()
                .map_err(|error| AudioOutputDeviceError::DeviceNameUnavailable {
                    message: error.to_string(),
                })?;
        if display_name != requested_name {
            continue;
        }
        if matched_duplicate_index == requested_duplicate_index {
            return Ok(device);
        }
        matched_duplicate_index += 1;
    }

    Err(AudioOutputDeviceError::DeviceUnavailable {
        stable_id: stable_id.to_string(),
    })
}

/// Делает best-effort stable id из CPAL 0.15 display name.
///
/// CPAL 0.15 не предоставляет backend `DeviceId`. Поэтому id из этой функции
/// стабилен только пока backend возвращает то же имя и порядок одноимённых
/// устройств. Когда проект перейдёт на CPAL с `DeviceId`, boundary останется тем же.
fn cpal_015_stable_id_from_name(display_name: &str, duplicate_index: usize) -> String {
    let escaped_name = escape_device_name_for_id(display_name);
    if duplicate_index == 0 {
        format!("{CPAL_015_NAME_ID_PREFIX}{escaped_name}")
    } else {
        format!("{CPAL_015_NAME_ID_PREFIX}{escaped_name}#{duplicate_index}")
    }
}

/// Разбирает best-effort CPAL 0.15 id обратно в имя и индекс одноимённого device.
fn parse_cpal_015_name_id(stable_id: &str) -> Option<(String, usize)> {
    let encoded = stable_id.strip_prefix(CPAL_015_NAME_ID_PREFIX)?;
    let (encoded_name, duplicate_index) = match encoded.rsplit_once('#') {
        Some((name, suffix)) => match suffix.parse::<usize>() {
            Ok(index) => (name, index),
            Err(_) => (encoded, 0),
        },
        None => (encoded, 0),
    };
    Some((unescape_device_name_from_id(encoded_name)?, duplicate_index))
}

/// Минимальный percent-escape для config-safe id без дополнительной зависимости.
fn escape_device_name_for_id(display_name: &str) -> String {
    let mut escaped = String::new();
    for byte in display_name.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                escaped.push(byte as char);
            }
            _ => escaped.push_str(&format!("%{byte:02X}")),
        }
    }
    escaped
}

/// Обратная операция для `escape_device_name_for_id`.
fn unescape_device_name_from_id(encoded_name: &str) -> Option<String> {
    let bytes = encoded_name.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }

        let high = *bytes.get(index + 1)?;
        let low = *bytes.get(index + 2)?;
        let value = hex_value(high)? * 16 + hex_value(low)?;
        decoded.push(value);
        index += 3;
    }

    String::from_utf8(decoded).ok()
}

/// Возвращает значение hex digit-а.
const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AudioOutputDeviceController, AudioOutputDeviceSelectionChange,
        DEFAULT_AUDIO_OUTPUT_DEVICE_ID, cpal_015_stable_id_from_name, parse_cpal_015_name_id,
    };

    #[test]
    fn controller_preserves_selected_stable_id_without_cpal_types() {
        let controller = AudioOutputDeviceController::new(DEFAULT_AUDIO_OUTPUT_DEVICE_ID);

        let change = controller
            .select_output_device("cpal-0.15-name:USB%20DAC")
            .expect("stable id должен примениться");

        assert_eq!(change, AudioOutputDeviceSelectionChange::Applied);
        assert_eq!(
            controller
                .selected_device_id()
                .expect("selected id должен читаться"),
            "cpal-0.15-name:USB%20DAC"
        );
    }

    #[test]
    fn cpal_015_name_id_roundtrips_spaces_hashes_and_unicode() {
        let stable_id = cpal_015_stable_id_from_name("USB DAC #1 Я", 2);

        let (display_name, duplicate_index) =
            parse_cpal_015_name_id(&stable_id).expect("id должен roundtrip-иться");

        assert_eq!(display_name, "USB DAC #1 Я");
        assert_eq!(duplicate_index, 2);
    }
}
