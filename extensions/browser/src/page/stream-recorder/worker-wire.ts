// Договор между записью главного потока и записью внутри воркера (поверх личного порта канала воркера).
//
// Воркер копит события и байты у себя и отправляет пачками: по таймеру и по просьбе главного потока
// (`recorder_flush` перед выгрузкой). Просьба несёт жетон, пачка-ответ возвращает его, так выгрузка
// знает, что воркер сдал всё, что успел записать к моменту просьбы.

import { viewOfAppendSource, type BinaryIntrinsics } from "../binary-view.ts";
import type { RecordedEvent } from "./format.ts";
import type { WorkerBatch } from "./recorder.ts";

export const FLUSH_COMMAND_KIND = "recorder_flush";
export const BATCH_REPORT_KIND = "recorder_batch";

/** Главный поток → воркер: сдай накопленное. */
export interface FlushCommand {
  readonly kind: typeof FLUSH_COMMAND_KIND;
  readonly token: number;
}

/** Воркер → главный поток: пачка. `events_json` — события одной строкой (структурное клонирование не нужно). */
export interface BatchReport {
  readonly kind: typeof BATCH_REPORT_KIND;
  /** Жетон просьбы, на которую это ответ; `null` — плановая пачка по таймеру. */
  readonly token: number | null;
  readonly time_origin: number;
  readonly events_json: string;
  readonly bytes: Uint8Array | null;
  readonly truncated: boolean;
}

export function parseFlushCommand(data: unknown): FlushCommand | undefined {
  if (typeof data !== "object" || data === null) return undefined;
  const { kind, token } = data as { readonly kind?: unknown; readonly token?: unknown };
  return kind === FLUSH_COMMAND_KIND && typeof token === "number" ? { kind, token } : undefined;
}

/** Разобранная пачка и жетон ответа. */
export interface ParsedBatch {
  readonly token: number | null;
  readonly batch: WorkerBatch;
}

/** Разбор пачки (сторона главного потока); `undefined` — не пачка или форма не та. */
export function parseBatchReport(data: unknown, binary: BinaryIntrinsics, parseJson: (text: string) => unknown): ParsedBatch | undefined {
  if (typeof data !== "object" || data === null) return undefined;
  const report = data as Partial<Record<keyof BatchReport, unknown>>;
  if (report.kind !== BATCH_REPORT_KIND) return undefined;
  const { token, time_origin: timeOrigin, events_json: eventsJson, bytes, truncated } = report;
  if ((token !== null && typeof token !== "number") || typeof timeOrigin !== "number" || typeof eventsJson !== "string" || typeof truncated !== "boolean") return undefined;
  const events = parseJson(eventsJson);
  if (!Array.isArray(events)) return undefined;
  const view = bytes === null ? null : viewOfAppendSource(binary, bytes);
  if (bytes !== null && view === null) return undefined;
  return { token, batch: { timeOrigin, events: events as RecordedEvent[], bytes: view, truncated } };
}
