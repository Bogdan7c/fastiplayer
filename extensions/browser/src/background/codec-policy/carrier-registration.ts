// Регистрация динамического контент-скрипта-курьера под действующую политику.
//
// Курьер (`src/content/codec-carrier.ts`) — по файлу на набор разрешённых семейств. Регистрация
// ровно одна (`CARRIER_SCRIPT_ID`) или её нет вовсе (прозрачная политика). Операция идемпотентна и
// самодостаточна: смотрит, что зарегистрировано сейчас, и приводит к желаемому — поэтому её можно
// звать при каждом старте фона, смене настройки и смене метки, не помня прошлого состояния.
import { CARRIER_SCRIPT_ID, carrierFileName, type CodecPolicy } from "../../shared/codec-policy.ts";

/** Описание зарегистрированного скрипта (то, что нужно нам). */
export interface RegisteredCarrier {
  readonly id: string;
  readonly files: readonly string[];
}

/** Минимум от `chrome.scripting`, нужный регистрации (в тестах — фейк с памятью). */
export interface CarrierScriptRegistry {
  list(): Promise<readonly RegisteredCarrier[]>;
  register(script: { readonly id: string; readonly file: string }): Promise<void>;
  unregister(id: string): Promise<void>;
}

/** Что сделала синхронизация (для тестов и диагностики). */
export type CarrierSyncOutcome = "registered" | "replaced" | "unregistered" | "unchanged";

export async function syncCarrierRegistration(registry: CarrierScriptRegistry, policy: CodecPolicy): Promise<CarrierSyncOutcome> {
  const current = (await registry.list()).find((script) => script.id === CARRIER_SCRIPT_ID);
  if (policy.kind === "transparent") {
    if (current === undefined) return "unchanged";
    await registry.unregister(CARRIER_SCRIPT_ID);
    return "unregistered";
  }
  const wantedFile = carrierFileName(policy.families);
  if (current !== undefined && current.files.length === 1 && current.files[0] === wantedFile) return "unchanged";
  if (current !== undefined) await registry.unregister(CARRIER_SCRIPT_ID);
  await registry.register({ id: CARRIER_SCRIPT_ID, file: wantedFile });
  return current === undefined ? "registered" : "replaced";
}
