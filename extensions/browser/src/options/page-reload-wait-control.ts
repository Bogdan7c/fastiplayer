// Раздел «Page reload» на странице настроек (сессия 16c, решение 305): сколько секунд окно плеера ждёт видео
// после перезагрузки страницы или перехода на другую страницу того же сайта. Чтение и запись — через
// `shared/page-reload-wait-settings.ts`; здесь только разметка, подписи и проверка ввода.
import type { KeyValueStore } from "../shared/codec-storage.ts";
import {
  MAX_PAGE_RELOAD_WAIT_SECONDS,
  MIN_PAGE_RELOAD_WAIT_SECONDS,
  parsePageReloadWaitInput,
  readPageReloadWaitSettings,
  writePageReloadWaitSettings,
} from "../shared/page-reload-wait-settings.ts";

/** Пояснение раздела: что значит срок и что значит `0`. */
export const PAGE_RELOAD_WAIT_EXPLANATION =
  "If the page reloads or opens another page of the same site, Fastiplayer keeps the window open this long for the new video. 0 — end at once.";

export const PAGE_RELOAD_WAIT_HINT = `Enter a whole number of seconds from ${MIN_PAGE_RELOAD_WAIT_SECONDS} to ${MAX_PAGE_RELOAD_WAIT_SECONDS}.`;

/** Нарисовать раздел в `container` и связать его с хранилищем. Ошибки хранилища показываем текстом, не глотаем. */
export async function mountPageReloadWaitControl(container: HTMLElement, store: KeyValueStore): Promise<void> {
  const heading = document.createElement("h2");
  heading.textContent = "Page reload";
  const label = document.createElement("label");
  const labelText = document.createElement("span");
  labelText.textContent = "Wait for the video after the page reloads (seconds): ";
  const input = document.createElement("input");
  input.type = "number";
  input.id = "page-reload-wait-seconds";
  input.min = String(MIN_PAGE_RELOAD_WAIT_SECONDS);
  input.max = String(MAX_PAGE_RELOAD_WAIT_SECONDS);
  label.append(labelText, input);
  const explanation = document.createElement("p");
  explanation.id = "page-reload-wait-hint";
  explanation.textContent = PAGE_RELOAD_WAIT_EXPLANATION;
  const status = document.createElement("p");
  status.id = "page-reload-wait-status";
  container.append(heading, label, explanation, status);

  try {
    input.value = String((await readPageReloadWaitSettings(store)).seconds);
  } catch (error) {
    status.textContent = `Could not read the setting: ${String(error)}`;
  }

  input.addEventListener("change", () => {
    const seconds = parsePageReloadWaitInput(input.value);
    if (seconds === undefined) {
      status.textContent = PAGE_RELOAD_WAIT_HINT;
      return;
    }
    // Сеанс читает срок при открытии: уже идущая передача живёт прежним сроком.
    writePageReloadWaitSettings(store, { seconds }).then(
      () => (status.textContent = "Saved. The new wait applies from the next video sent to the player."),
      (error: unknown) => (status.textContent = `Could not save: ${String(error)}`),
    );
  });
}
