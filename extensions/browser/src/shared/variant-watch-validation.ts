// Строгий разбор сообщений наблюдения за вариантами (сессия 14b). Форма проверяется на обоих краях, как у
// рекламы (`ad-watch-validation.ts`): фрейм разбирает просьбы фона, фон — донесения и ответы фрейма.

import {
  ARM_VARIANT_WATCH_KIND,
  MAX_REPORTED_ADAPTER_LENGTH,
  MAX_REPORTED_KEY_LENGTH,
  MAX_REPORTED_VARIANT_ID_LENGTH,
  MAX_REPORTED_VARIANT_LABEL_LENGTH,
  MAX_REPORTED_VARIANT_OPTIONS,
  MAX_VARIANT_WATCH_LEASE_MS,
  VARIANT_COMMAND_KIND,
  VARIANT_WATCH_REPORT_KIND,
  type ArmVariantWatchRequest,
  type PageVariantDescription,
  type VariantActionOutcome,
  type VariantCommand,
  type VariantCommandRequest,
  type VariantWatchReport,
} from "./variant-watch-contract.ts";
import type { PageVariantOption } from "../adapters/capabilities/variants/contract.ts";

type PlainRecord = Readonly<Record<string, unknown>>;

function isPlainRecord(value: unknown): value is PlainRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value) && Object.getPrototypeOf(value) === Object.prototype;
}

function exactRecord(value: unknown, keys: readonly string[]): PlainRecord | undefined {
  if (!isPlainRecord(value)) return undefined;
  const own = Object.keys(value);
  return own.length === keys.length && keys.every((key) => Object.hasOwn(value, key)) ? value : undefined;
}

function isBoundedString(value: unknown, minLength: number, maxLength: number): value is string {
  return typeof value === "string" && value.length >= minLength && value.length <= maxLength;
}

function isKeyOrNull(value: unknown): value is string | null {
  return value === null || isBoundedString(value, 1, MAX_REPORTED_KEY_LENGTH);
}

function parseOption(value: unknown): PageVariantOption | undefined {
  // Пометка `automatic` необязательна (14b2): без неё — ровно три поля, с ней — четыре.
  const record = exactRecord(value, ["id", "label", "selected"]) ?? exactRecord(value, ["id", "label", "selected", "automatic"]);
  if (record === undefined) return undefined;
  const { id, label, selected, automatic } = record;
  if (!isBoundedString(id, 1, MAX_REPORTED_VARIANT_ID_LENGTH)) return undefined;
  if (!isBoundedString(label, 0, MAX_REPORTED_VARIANT_LABEL_LENGTH) || typeof selected !== "boolean") return undefined;
  if (automatic === undefined) return { id, label, selected };
  return typeof automatic === "boolean" ? { id, label, selected, automatic } : undefined;
}

function parseOptions(value: unknown): readonly PageVariantOption[] | undefined {
  if (!Array.isArray(value) || value.length > MAX_REPORTED_VARIANT_OPTIONS) return undefined;
  const options: PageVariantOption[] = [];
  for (const item of value as readonly unknown[]) {
    const option = parseOption(item);
    if (option === undefined) return undefined;
    options.push(option);
  }
  return options;
}

/** Описание меню страницы по форме. */
export function parsePageVariantDescription(value: unknown): PageVariantDescription | undefined {
  const record = exactRecord(value, ["seriesKey", "episodeKey", "audio", "quality", "nextEpisode"]);
  if (record === undefined) return undefined;
  const { seriesKey, episodeKey, nextEpisode } = record;
  if (!isKeyOrNull(seriesKey) || !isKeyOrNull(episodeKey) || typeof nextEpisode !== "boolean") return undefined;
  const audio = parseOptions(record["audio"]);
  const quality = parseOptions(record["quality"]);
  if (audio === undefined || quality === undefined) return undefined;
  return { seriesKey, episodeKey, audio, quality, nextEpisode };
}

/** Донесение фрейма (разбирает фон). */
export function parseVariantWatchReport(value: unknown): VariantWatchReport | undefined {
  const record = exactRecord(value, ["kind", "adapter", "description"]);
  if (record === undefined || record["kind"] !== VARIANT_WATCH_REPORT_KIND) return undefined;
  const adapter = record["adapter"];
  if (!isBoundedString(adapter, 1, MAX_REPORTED_ADAPTER_LENGTH)) return undefined;
  if (record["description"] === null) return { kind: VARIANT_WATCH_REPORT_KIND, adapter, description: null };
  const description = parsePageVariantDescription(record["description"]);
  return description === undefined ? undefined : { kind: VARIANT_WATCH_REPORT_KIND, adapter, description };
}

/** Просьба фона взять наблюдение в аренду (разбирает фрейм). */
export function parseArmVariantWatchRequest(value: unknown): ArmVariantWatchRequest | undefined {
  const record = exactRecord(value, ["kind", "lease_ms"]);
  if (record === undefined || record["kind"] !== ARM_VARIANT_WATCH_KIND) return undefined;
  const leaseMs = record["lease_ms"];
  const valid = typeof leaseMs === "number" && Number.isSafeInteger(leaseMs) && leaseMs >= 1 && leaseMs <= MAX_VARIANT_WATCH_LEASE_MS;
  return valid ? { kind: ARM_VARIANT_WATCH_KIND, lease_ms: leaseMs } : undefined;
}

function parseCommand(value: unknown): VariantCommand | undefined {
  if (exactRecord(value, ["kind"])?.["kind"] === "next") return { kind: "next" };
  const record = exactRecord(value, ["kind", "variant_kind", "variant_id"]);
  if (record === undefined || record["kind"] !== "select") return undefined;
  const { variant_kind: variantKind, variant_id: variantId } = record;
  if ((variantKind !== "audio" && variantKind !== "quality") || !isBoundedString(variantId, 1, MAX_REPORTED_VARIANT_ID_LENGTH)) return undefined;
  return { kind: "select", variant_kind: variantKind, variant_id: variantId };
}

/** Команда фона (разбирает фрейм). */
export function parseVariantCommandRequest(value: unknown): VariantCommandRequest | undefined {
  const record = exactRecord(value, ["kind", "command"]);
  if (record === undefined || record["kind"] !== VARIANT_COMMAND_KIND) return undefined;
  const command = parseCommand(record["command"]);
  return command === undefined ? undefined : { kind: VARIANT_COMMAND_KIND, command };
}

const ACTION_OUTCOMES: readonly VariantActionOutcome[] = ["done", "no_such_variant", "person_only", "failed"];

/** Ответ фрейма на команду (разбирает фон). Незнакомый ответ — `undefined`. */
export function parseVariantActionOutcome(value: unknown): VariantActionOutcome | undefined {
  return ACTION_OUTCOMES.find((outcome) => outcome === value);
}
