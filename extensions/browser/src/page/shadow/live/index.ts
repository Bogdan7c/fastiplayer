// Тень эфира HLS (сессия 10a3, решение владельца 232): сборка времени источника с видом для страницы.
//
// Кто чем владеет:
// - `LiveClockAlignment` — сдвиг между часами плеера и временем страницы (единственный владелец);
// - `LiveHlsTimeSource` — последнее окно эфира, перевод моментов, окно перемотки страницы, возврат на край;
// - `LiveHlsSourceView` — `duration`/`seekable`/`buffered`/`networkState`, которые видит страница.
// Снятие закачки, события настоящего элемента и перезагрузка `src` при возврате — общие с тенью файла
// (`file-shadow-lifecycle.ts`, `file-return.ts`).

import type { MediaTimeMicros } from "../../../protocol/time.ts";
import { LiveHlsSourceView, type LiveViewInputs } from "./live-hls-source-view.ts";
import { LiveHlsTimeSource } from "./live-hls-time-source.ts";

export type { LiveViewInputs } from "./live-hls-source-view.ts";
export { LiveHlsTimeSource } from "./live-hls-time-source.ts";

/** Время тени эфира; `handoffPageUs` — секунда страницы в момент передачи (P0). */
export function createLiveHlsTimeSource(handoffPageUs: MediaTimeMicros, inputs: LiveViewInputs): LiveHlsTimeSource {
  return new LiveHlsTimeSource(handoffPageUs, (source, snapshot) => new LiveHlsSourceView(source, snapshot, inputs));
}
