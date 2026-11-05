// Строгий разбор сообщений слежения за рекламой (сессия 09d). Форма проверяется на обоих краях:
// фрейм разбирает просьбы фона, фон — донесения фрейма (защита в глубину, как у остальных сообщений).
// Нарушение лимитов протокола на проводе — ошибка сеанса, поэтому фон не имеет права их пропустить.

import { MAX_AD_COUNTDOWN_SECONDS, MAX_ADS_IN_POD, type AdPod, type AdSkip, type PageAdState } from "../protocol/index.ts";
import {
  AD_WATCH_REPORT_KIND,
  ARM_AD_WATCH_KIND,
  MAX_AD_WATCH_LEASE_MS,
  PRESS_AD_SKIP_KIND,
  type AdWatchReport,
  type ArmAdWatchRequest,
  type PressAdSkipRequest,
  type PressSkipOutcome,
} from "./ad-watch-contract.ts";

type PlainRecord = Readonly<Record<string, unknown>>;

function isPlainRecord(value: unknown): value is PlainRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value) && Object.getPrototypeOf(value) === Object.prototype;
}

function hasExactKeys(record: PlainRecord, keys: readonly string[]): boolean {
  const own = Object.keys(record);
  return own.length === keys.length && keys.every((key) => Object.hasOwn(record, key));
}

function isIntegerInRange(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= min && value <= max;
}

function parseAdPod(value: unknown): AdPod | undefined {
  if (!isPlainRecord(value)) return undefined;
  if (value["kind"] === "unknown") return hasExactKeys(value, ["kind"]) ? { kind: "unknown" } : undefined;
  if (value["kind"] !== "known" || !hasExactKeys(value, ["kind", "index", "count"])) return undefined;
  const { index, count } = value;
  if (!isIntegerInRange(index, 1, MAX_ADS_IN_POD) || !isIntegerInRange(count, 1, MAX_ADS_IN_POD) || index > count) return undefined;
  return { kind: "known", index, count };
}

function parseAdSkip(value: unknown): AdSkip | undefined {
  if (!isPlainRecord(value)) return undefined;
  switch (value["kind"]) {
    case "unavailable":
    case "available":
    case "on_page":
      return hasExactKeys(value, ["kind"]) ? { kind: value["kind"] } : undefined;
    case "countdown": {
      const seconds = value["seconds"];
      return hasExactKeys(value, ["kind", "seconds"]) && isIntegerInRange(seconds, 0, MAX_AD_COUNTDOWN_SECONDS)
        ? { kind: "countdown", seconds }
        : undefined;
    }
    default:
      return undefined;
  }
}

/** Состояние рекламы строго по протоколу: числа в пределах, номер не больше размера блока. */
export function parsePageAdState(value: unknown): PageAdState | undefined {
  if (!isPlainRecord(value)) return undefined;
  if (value["kind"] === "none") return hasExactKeys(value, ["kind"]) ? { kind: "none" } : undefined;
  if (value["kind"] !== "showing" || !hasExactKeys(value, ["kind", "pod", "skip"])) return undefined;
  const pod = parseAdPod(value["pod"]);
  const skip = parseAdSkip(value["skip"]);
  return pod === undefined || skip === undefined ? undefined : { kind: "showing", pod, skip };
}

/** Донесение фрейма о рекламе. */
export function parseAdWatchReport(value: unknown): AdWatchReport | undefined {
  if (!isPlainRecord(value) || value["kind"] !== AD_WATCH_REPORT_KIND || !hasExactKeys(value, ["kind", "ad"])) return undefined;
  const ad = parsePageAdState(value["ad"]);
  return ad === undefined ? undefined : { kind: AD_WATCH_REPORT_KIND, ad };
}

/** Просьба фона взять наблюдение в аренду. */
export function parseArmAdWatchRequest(value: unknown): ArmAdWatchRequest | undefined {
  if (!isPlainRecord(value) || value["kind"] !== ARM_AD_WATCH_KIND || !hasExactKeys(value, ["kind", "lease_ms"])) return undefined;
  const leaseMs = value["lease_ms"];
  return isIntegerInRange(leaseMs, 1, MAX_AD_WATCH_LEASE_MS) ? { kind: ARM_AD_WATCH_KIND, lease_ms: leaseMs } : undefined;
}

/** Просьба фона нажать кнопку пропуска. */
export function parsePressAdSkipRequest(value: unknown): PressAdSkipRequest | undefined {
  return isPlainRecord(value) && value["kind"] === PRESS_AD_SKIP_KIND && hasExactKeys(value, ["kind"]) ? { kind: PRESS_AD_SKIP_KIND } : undefined;
}

/** Итог нажатия из ответа фрейма; чужой ответ — `undefined`. */
export function parsePressSkipOutcome(value: unknown): PressSkipOutcome | undefined {
  return value === "pressed" || value === "no_button" || value === "person_only" ? value : undefined;
}
