// Проводка политики кодеков к настоящему браузерному API (`chrome.storage`, `chrome.scripting`).
// Единственное место фона, где эти API нужны политике; остальное тестируется на фейках.
import { browserApi } from "../../shared/browser-api.ts";
import { CARRIER_SCRIPT_ID } from "../../shared/codec-policy.ts";
import type { KeyValueStore } from "../../shared/codec-storage.ts";
import type { CarrierScriptRegistry } from "./carrier-registration.ts";
import { createCodecPolicyService, type CodecPolicyService } from "./codec-policy-service.ts";

function createBrowserStore(): KeyValueStore {
  return {
    get: (keys) => browserApi.storage.local.get([...keys]),
    set: (items) => browserApi.storage.local.set(items),
  };
}

function createBrowserRegistry(): CarrierScriptRegistry {
  return {
    async list() {
      const scripts = await browserApi.scripting.getRegisteredContentScripts({ ids: [CARRIER_SCRIPT_ID] });
      return scripts.map((script) => ({ id: script.id, files: script.js ?? [] }));
    },
    register: ({ id, file }) =>
      browserApi.scripting.registerContentScripts([
        {
          id,
          js: [file],
          matches: ["<all_urls>"],
          allFrames: true,
          runAt: "document_start",
          // Как у `match_about_blank` + `match_origin_as_fallback` статических скриптов: about:blank,
          // data: и blob:-фреймы с унаследованным origin тоже получают курьера.
          matchOriginAsFallback: true,
          // Регистрация переживает перезапуск браузера: первая страница после старта браузера не ждёт фон.
          persistAcrossSessions: true,
          // Мир не задаём: по умолчанию ISOLATED — значения политики страница не видит.
        },
      ]),
    unregister: (id) => browserApi.scripting.unregisterContentScripts({ ids: [id] }),
  };
}

/**
 * Запустить политику кодеков в фоне: пересчёт на каждый старт фона и на каждое изменение настройки
 * или метки в `storage.local`. Слушатель ставится синхронно на верхнем уровне (требование MV3).
 */
export function startCodecPolicy(): CodecPolicyService {
  const service = createCodecPolicyService({
    store: createBrowserStore(),
    registry: createBrowserRegistry(),
    warn: (message) => console.warn(`[codec-policy] ${message}`),
  });
  browserApi.storage.onChanged.addListener((changes, areaName) => {
    if (areaName === "local" && Object.keys(changes).some(service.isPolicyInputKey)) void service.resync();
  });
  void service.resync();
  return service;
}
