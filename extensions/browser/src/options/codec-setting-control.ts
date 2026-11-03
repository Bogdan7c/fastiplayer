// Настройка «Video codec for sites»: выбор из трёх значений, сохраняется в `storage.local`.
// Разметка и подписи — здесь; чтение и запись — через `shared/codec-storage.ts`. Оформление отложено
// до финальной доводки интерфейса (решение владельца).
import { CODEC_SETTINGS, type CodecSetting } from "../shared/codec-policy.ts";
import { readCodecSetting, writeCodecSetting, type KeyValueStore } from "../shared/codec-storage.ts";

const SETTING_LABELS: Record<CodecSetting, string> = {
  auto: "Auto (follow Fastiplayer)",
  prefer_vp9: "Prefer VP9 (no AV1)",
  always_h264: "Always H.264",
  off: "Off",
};

/** Нарисовать выбор в `container` и связать его с хранилищем. Ошибки хранилища показываем текстом, не глотаем. */
export async function mountCodecSettingControl(container: HTMLElement, store: KeyValueStore): Promise<void> {
  const label = document.createElement("label");
  label.textContent = "Video codec for sites: ";
  const select = document.createElement("select");
  select.id = "codec-setting";
  for (const setting of CODEC_SETTINGS) {
    const option = document.createElement("option");
    option.value = setting;
    option.textContent = SETTING_LABELS[setting];
    select.append(option);
  }
  label.append(select);
  const hint = document.createElement("p");
  hint.id = "codec-setting-hint";
  hint.textContent = "Auto follows the Video codec priority and renderer of the Fastiplayer on this computer (applies from its next browser session). Takes effect on pages opened afterwards.";
  const status = document.createElement("p");
  status.id = "codec-setting-status";
  container.append(label, hint, status);

  try {
    select.value = await readCodecSetting(store);
  } catch (error) {
    status.textContent = `Could not read the setting: ${String(error)}`;
  }
  select.addEventListener("change", () => {
    const chosen = CODEC_SETTINGS.find((setting) => setting === select.value);
    if (chosen === undefined) return;
    writeCodecSetting(store, chosen).then(
      () => (status.textContent = "Saved."),
      (error: unknown) => (status.textContent = `Could not save: ${String(error)}`),
    );
  });
}
