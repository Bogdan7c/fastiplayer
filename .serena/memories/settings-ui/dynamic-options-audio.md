# Settings UI dynamic options + audio output devices

S12 implemented dynamic option providers for `audio.output_device`.

Architecture:
- `settings-core` keeps neutral provider contracts and UI-visible option snapshots. `SettingOptionsStatus` now includes `Loading`; unavailable current values remain explicit through `SettingOptionCurrentValue::UnavailableCurrent`.
- `app-egui::settings_runtime` owns `option_providers` and `option_cache`. Visual settings UI never calls providers directly; it only renders cached `SettingOptions` and emits `SettingsUiAction::RefreshOptions { provider_id }`.
- Dynamic option refresh happens on settings window open, when selecting a section containing dynamic providers, and through the explicit refresh action. Provider failures are converted into `SettingOptionsStatus::Unavailable` with an option-provider error message; they must not break the whole settings window.
- `audio` owns CPAL output device enumeration and selection. Public owner API: `AudioOutputDeviceController`, `AudioOutputDeviceInfo`, `AudioOutputDeviceSelectionChange`, `AudioOutputDeviceError`, `DEFAULT_AUDIO_OUTPUT_DEVICE_ID`, and `list_output_devices()`. CPAL types stay private to `audio`.
- `CpalAudioOutputFactory::new(controller)` receives the shared audio device controller from settings runtime. `AudioOutput::new_with_device_id(...)` resolves the selected stable id inside `audio`; `AudioOutput::new(...)` still uses the default output device.
- `fastiplayer-settings` routes `audio.output_device` through `PlayerCommittedSettingsUpdate.audio_output_device_id` and `AppRuntimeRouteGroup::PlayerAudioOutputDevice`, not through deferred boundary settings.

Important limitation:
- The project currently uses CPAL 0.15.3, which does not expose backend `DeviceId`. The current stable id scheme is best-effort: `default` for system default, otherwise `cpal-0.15-name:<percent-escaped-display-name>[#duplicate-index]`. It is stable while the backend returns the same device name/order. A future CPAL upgrade with real device IDs should only require changes inside `audio::devices`, preserving the neutral boundary.

Focused tests cover:
- saved unavailable `audio.output_device` is preserved and shown as current unavailable;
- provider errors become option-provider error snapshots without breaking the settings window;
- visual UI can render unavailable current dynamic option;
- manual refresh updates cached dynamic options;
- selected available device is passed through the audio owner boundary;
- `audio` crate has no `app-egui` dependency.