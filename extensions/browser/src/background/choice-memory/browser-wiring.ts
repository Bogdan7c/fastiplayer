// Сборка памяти выбора (сессия 14b2) на настоящих API браузера: хранилище — `storage.local`, просьбы
// страницы настроек — `runtime.onMessage`. Логика — в модулях этой папки, здесь только переходники.

import { browserApi } from "../../shared/browser-api.ts";
import { createChoiceMemoryCommandListener } from "./choice-memory-commands.ts";
import { createChoiceMemoryStore, type ChoiceMemoryStore } from "./choice-memory-store.ts";

/** Создать единственное хранилище памяти фона и принять просьбы страницы настроек. */
export function installChoiceMemory(): ChoiceMemoryStore {
  const memory = createChoiceMemoryStore({
    store: { get: (keys) => browserApi.storage.local.get([...keys]), set: (items) => browserApi.storage.local.set(items) },
    now: () => Date.now(),
  });
  // Слушатель ставится синхронно при старте фона: просьба страницы настроек будит фон и находит его.
  browserApi.runtime.onMessage.addListener(
    createChoiceMemoryCommandListener({ extensionId: browserApi.runtime.id, extensionBaseUrl: browserApi.runtime.getURL("") }, memory),
  );
  return memory;
}
