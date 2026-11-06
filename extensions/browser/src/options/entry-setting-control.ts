// Настройки входов (сессия 10c): три чекбокса «как отдать видео плееру». Применяются сразу, без кнопки Save,
// как соседние настройки. Чтение и запись — через `shared/entry-settings.ts`; вид и подписи — здесь.
import type { KeyValueStore } from "../shared/codec-storage.ts";
import {
  DEFAULT_ENTRY_SETTINGS,
  ENTRY_SETTING_NAMES,
  readEntrySettings,
  writeEntrySettings,
  type EntrySettingName,
  type EntrySettings,
} from "../shared/entry-settings.ts";

const SETTING_LABELS: Record<EntrySettingName, string> = {
  video_menu: "Right-click on a video or page",
  link_menu: "Right-click on a link",
  overlay_button: "Button over videos",
};

/** Изменить ровно одно поле, остальные взять как есть (то, что лежит в хранилище сейчас, а не что видела эта вкладка). */
export function withEntrySetting(current: EntrySettings, name: EntrySettingName, value: boolean): EntrySettings {
  return { ...current, [name]: value };
}

/** Нарисовать чекбоксы в `container` и связать их с хранилищем. Ошибки хранилища показываем текстом, не глотаем. */
export async function mountEntrySettingControl(container: HTMLElement, store: KeyValueStore): Promise<void> {
  const heading = document.createElement("h2");
  heading.textContent = "Ways to play a video in Fastiplayer";
  const status = document.createElement("p");
  status.id = "entry-setting-status";
  const checkboxes = new Map<EntrySettingName, HTMLInputElement>();
  const rows: HTMLElement[] = [];
  for (const name of ENTRY_SETTING_NAMES) {
    const label = document.createElement("label");
    label.style.display = "block";
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.id = `entry-setting-${name}`;
    checkbox.checked = DEFAULT_ENTRY_SETTINGS[name];
    label.append(checkbox, ` ${SETTING_LABELS[name]}`);
    checkboxes.set(name, checkbox);
    rows.push(label);
  }
  container.append(heading, ...rows, status);

  try {
    const stored = await readEntrySettings(store);
    for (const [name, checkbox] of checkboxes) checkbox.checked = stored[name];
  } catch (error) {
    status.textContent = `Could not read the settings: ${String(error)}`;
  }

  for (const [name, checkbox] of checkboxes) {
    checkbox.addEventListener("change", () => {
      // Перечитываем перед записью: другая вкладка настроек могла поменять соседние поля. Не прочли — не пишем
      // (иначе затёрли бы соседей значениями по умолчанию).
      readEntrySettings(store)
        .then((current) => writeEntrySettings(store, withEntrySetting(current, name, checkbox.checked)))
        .then(
          () => (status.textContent = "Saved."),
          (error: unknown) => (status.textContent = `Could not save: ${String(error)}`),
        );
    });
  }
}
