// Сборка модели вкладки на настоящих API браузера.
import { browserApi } from "../../shared/browser-api.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createMemoryTabModelStore, createSessionStorageTabModelStore, type TabModelStore } from "./session-store.ts";
import { followTabLifecycle } from "./tab-lifecycle.ts";
import { createTabModel, type TabModel } from "./tab-model.ts";

/** `storage.session` (Chrome 102+, Firefox 115+); без него — память: модель всё равно работает как кэш. */
function pickStore(): TabModelStore {
  const area: chrome.storage.StorageArea | undefined = browserApi.storage?.session;
  if (area === undefined) {
    console.warn("[fastiplayer] storage.session is unavailable, the tab model lives in memory only");
    return createMemoryTabModelStore();
  }
  return createSessionStorageTabModelStore(area);
}

export function installTabModel(scheduler: Scheduler): TabModel {
  const model = createTabModel({
    store: pickStore(),
    now: () => scheduler.now(),
    reportStoreFailure: (error) => console.warn("[fastiplayer] the tab model store failed, continuing without persistence:", error),
  });
  followTabLifecycle(model, browserApi.tabs, (error) => console.warn("[fastiplayer] could not list the open tabs, keeping the tab model as it is:", error));
  return model;
}
