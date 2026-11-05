// Настройка «Local network videos from internet pages» (решение 199): выбор из двух значений,
// сохраняется в `storage.local`. Разметка и подписи — здесь; чтение и запись — через
// `shared/local-address-setting.ts`. Вопрос человеку рисует попап сессии 10.
import type { KeyValueStore } from "../shared/codec-storage.ts";
import {
  LOCAL_ADDRESS_SETTINGS,
  readLocalAddressSetting,
  writeLocalAddressSetting,
  type LocalAddressSetting,
} from "../shared/local-address-setting.ts";

const SETTING_LABELS: Record<LocalAddressSetting, string> = {
  block: "Block",
  ask: "Ask",
};

/** Нарисовать выбор в `container` и связать его с хранилищем. Ошибки хранилища показываем текстом, не глотаем. */
export async function mountLocalAddressSettingControl(container: HTMLElement, store: KeyValueStore): Promise<void> {
  const label = document.createElement("label");
  label.textContent = "Local network videos from internet pages: ";
  const select = document.createElement("select");
  select.id = "local-address-setting";
  for (const setting of LOCAL_ADDRESS_SETTINGS) {
    const option = document.createElement("option");
    option.value = setting;
    option.textContent = SETTING_LABELS[setting];
    select.append(option);
  }
  label.append(select);
  const hint = document.createElement("p");
  hint.id = "local-address-setting-hint";
  hint.textContent = "A page on the internet asks for a video on your home network or this computer. Block keeps that video in the browser; Ask lets you decide each time. Pages that are themselves on your local network are not affected.";
  const status = document.createElement("p");
  status.id = "local-address-setting-status";
  container.append(label, hint, status);

  try {
    select.value = await readLocalAddressSetting(store);
  } catch (error) {
    status.textContent = `Could not read the setting: ${String(error)}`;
  }
  select.addEventListener("change", () => {
    const chosen = LOCAL_ADDRESS_SETTINGS.find((setting) => setting === select.value);
    if (chosen === undefined) return;
    writeLocalAddressSetting(store, chosen).then(
      () => (status.textContent = "Saved."),
      (error: unknown) => (status.textContent = `Could not save: ${String(error)}`),
    );
  });
}
