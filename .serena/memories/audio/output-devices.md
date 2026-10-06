# Audio output device owner API

`audio` owns output device enumeration and selection for settings/UI integration.

Public API:
- `AudioOutputDeviceController` stores the selected stable output-device id in shared state and exposes `selected_device_id()`, `select_output_device(...)`, and `output_devices()`.
- `AudioOutputDeviceInfo` is the neutral snapshot exposed outside `audio`: `stable_id`, `display_name`, `is_system_default`.
- `DEFAULT_AUDIO_OUTPUT_DEVICE_ID` is `default` and means CPAL/system default output.
- `CpalAudioOutputFactory::new(controller)` reads the shared controller when it creates a concrete output.
- `AudioOutput::new_with_device_id(...)` resolves the stable id inside `audio`; external crates must not receive CPAL device/host types.

Device request/route and stream health (UX edge cases session 10, 2026-10-06):
- Neutral contract lives in `audio-core/src/output_backend.rs` (moved there together with `AudioOutputFactory` and `PlayerAudioClock` because `audio-core/src/lib.rs` is a size-ratchet legacy file). `AudioOutputFactory::create_output(spec, AudioOutputDeviceRequest::{SelectedOnly, SelectedOrSystemDefault, SystemDefault}) -> CreatedAudioOutput { output, route: AudioOutputDeviceRoute::{SelectedDevice, SystemDefault, SystemDefaultInsteadOfUnavailable { unavailable_device_name }} }`. `PlayerAudioOutput::stream_health() -> AudioOutputStreamHealth::{Running, Failed}` (default `Running`).
- Attempt order is a CPAL-free function `audio::output_device_fallback::open_output_for_device_request` (selected → `default`; selected `default` is not retried; both causes in the error). Display name for the notice: `devices::output_device_display_name(stable_id)`. The controller selection (config) is only read, never changed by fallback.
- CPAL error callback only sets a lock-free `StreamFailureSignal` (`output/stream_callbacks.rs`, logs first error only). `StreamCallbackShared` bundles consumer/clock/channels/signal for `build_stream*`.
- Machine facts (CPAL 0.15.3 ALSA host on PipeWire): device list is ALSA PCMs (`pipewire`, `pulse`, `default`, `front:CARD=…`, `hdmi:…`); Bluetooth/network sinks are reachable only via `default`/`pipewire`. When a PipeWire sink disappears under a `pipewire`/`default` stream, PipeWire moves the stream and CPAL reports no error. Unplugging a directly selected ALSA device reports `StreamError::BackendSpecific` (POLLERR/ENODEV), not `DeviceNotAvailable`; xrun (EPIPE) is handled inside CPAL and never reaches the callback; the ALSA worker keeps looping after errors (error storm). Player reaction: `mem:player-core/audio-runtime`.

CPAL 0.15 limitation:
- Local dependency is CPAL 0.15.3. It has `DeviceTrait::name()` but no stable backend `DeviceId`, so `audio::devices` uses best-effort ids `cpal-0.15-name:<escaped-name>[#duplicate-index]` for non-default devices.
- Keep this limitation contained in `audio::devices`; settings-core, app-egui, and fastiplayer-settings should treat ids as opaque strings.