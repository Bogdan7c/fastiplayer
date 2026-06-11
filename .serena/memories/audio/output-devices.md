# Audio output device owner API

`audio` owns output device enumeration and selection for settings/UI integration.

Public API:
- `AudioOutputDeviceController` stores the selected stable output-device id in shared state and exposes `selected_device_id()`, `select_output_device(...)`, and `output_devices()`.
- `AudioOutputDeviceInfo` is the neutral snapshot exposed outside `audio`: `stable_id`, `display_name`, `is_system_default`.
- `DEFAULT_AUDIO_OUTPUT_DEVICE_ID` is `default` and means CPAL/system default output.
- `CpalAudioOutputFactory::new(controller)` reads the shared controller when it creates a concrete output.
- `AudioOutput::new_with_device_id(...)` resolves the stable id inside `audio`; external crates must not receive CPAL device/host types.

CPAL 0.15 limitation:
- Local dependency is CPAL 0.15.3. It has `DeviceTrait::name()` but no stable backend `DeviceId`, so `audio::devices` uses best-effort ids `cpal-0.15-name:<escaped-name>[#duplicate-index]` for non-default devices.
- Keep this limitation contained in `audio::devices`; settings-core, app-egui, and rustiplayer-settings should treat ids as opaque strings.