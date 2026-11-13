// Запись или эфир у нативного HLS — по `duration` настоящего элемента (сессия 10a3, решение 232).
//
// Одно правило на MAIN-мир: его спрашивают контроллер передачи (какое событие слать фону и какую тень ставить)
// и сторож следующего видео (какую тень ставить задержанному видео). Разойтись они не должны: иначе фон
// договорился бы с плеером об эфире, а на элементе стояла бы тень записи (или наоборот).

import { isEndlessDurationSeconds } from "../shared/endless-duration.ts";
import type { HlsStreamKind } from "../shared/handoff-contract.ts";
import type { ShadowSourceKind } from "./handoff-seam.ts";

/**
 * Вид нативного HLS по `duration` элемента (MDN `HTMLMediaElement.duration`): `+Infinity` или длиннее года —
 * эфир (решение 319), конечная — запись, NaN (метаданных нет) и прочее — `null`, передавать нечего.
 */
export function hlsStreamOfDuration(durationSeconds: number): HlsStreamKind | null {
  if (isEndlessDurationSeconds(durationSeconds)) return "live";
  return Number.isFinite(durationSeconds) ? "vod" : null;
}

/**
 * Какая тень у HLS этого вида: запись — тень файла (решения 186/187: снятая закачка, «весь ролик»,
 * возврат на T), эфир — тень эфира (длительность Infinity, окно `live_window`, возврат на край).
 */
export function shadowSourceOfHls(stream: HlsStreamKind): ShadowSourceKind {
  return stream === "live" ? "live_hls" : "file";
}
