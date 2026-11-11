// Возврат настоящего элемента странице при обычном конце тени (`end`): громкость, скорость,
// позиция, игра или пауза и исход ожидающих `play()`.
//
// Модуль не владеет состоянием тени: всё, что принадлежит контроллеру (обещания `play()`, очередь
// событий, колбэки кадров), он берёт через узкий шов `RealReturnPorts`. Так контроллер не растёт, а
// порядок возврата (позиция → колбэки → игра/пауза) закреплён в одном месте.

import type { PendingPlay } from "../handoff-seam.ts";
import { callOriginal, type ElementOriginals, readOriginal, writeOriginal } from "./originals.ts";
import { shadowProblemReporter } from "./once-reporter.ts";
import type { ReturnPosition } from "./shadow-time-source.ts";

const MICROS_PER_SECOND = 1_000_000;
/**
 * При возврате настоящий элемент переставляется, только если он дальше 1 мс от цели: лишняя
 * перестановка — это видимые странице `seeking`/`seeked` и декод кадров без нужды.
 */
const SAME_POSITION_TOLERANCE_US = 1_000;

/** Секунды → микросекунды; нечисло (NaN, бесконечность) — 0, как у контроллера тени. */
function secondsToMicros(seconds: number): number {
  return Number.isFinite(seconds) ? Math.round(seconds * MICROS_PER_SECOND) : 0;
}

/** Что возврату нужно от контроллера тени (его состояние остаётся у него). */
export interface RealReturnPorts {
  readonly element: HTMLMediaElement;
  readonly originals: ElementOriginals;
  /** Скорость, какой её видела страница: настоящий элемент получает именно её. */
  readonly pageRate: number;
  /** Забрать ожидающие `play()` страницы (список у контроллера очищается). */
  takePendingPlays(): PendingPlay[];
  /** Отвергнуть ожидающие `play()` (итог — пауза). */
  rejectPendingPlays(): void;
  /** Передать ожидающие `requestVideoFrameCallback` настоящему элементу. */
  handOverFrameCallbacks(): void;
  /** Поставить DOM-событие в очередь страницы. */
  enqueueEvent(type: string): void;
}

/** Возврат: скорость, позиция (у эфира — где браузер вошёл в эфир сам), игра/пауза. */
export function returnRealElement(ports: RealReturnPorts, position: ReturnPosition, resumePlaying: boolean, pageSawPlaying: boolean): void {
  const media = ports.originals.media;
  const element = ports.element;
  // `muted` страницы уже вернул `#detach` (он всегда раньше возврата позиции).
  if (Number(readOriginal(media.playbackRate, element)) !== ports.pageRate) {
    writeOriginal(media.playbackRate, element, ports.pageRate);
  }
  const realUs = secondsToMicros(Number(readOriginal(media.currentTime, element)));
  if (position.kind === "at" && Math.abs(realUs - position.targetUs) > SAME_POSITION_TOLERANCE_US) {
    writeOriginal(media.currentTime, element, position.targetUs / MICROS_PER_SECOND);
  }
  ports.handOverFrameCallbacks();
  if (resumePlaying) {
    resumeRealPlayback(ports, pageSawPlaying);
    return;
  }
  callOriginal(media.pause, element);
  ports.rejectPendingPlays();
  // Страница видела «играет», а итог — пауза: настоящий элемент уже стоял и сам `pause` не пошлёт.
  if (pageSawPlaying) {
    ports.enqueueEvent("timeupdate");
    ports.enqueueEvent("pause");
  }
}

/** Возврат с игрой: ожидающие `play()` страницы исполняются исходом настоящего `play()`. */
function resumeRealPlayback(ports: RealReturnPorts, pageSawPlaying: boolean): void {
  const waiting = ports.takePendingPlays();
  const media = ports.originals.media;
  // Эфир уже пошёл вместе с перезагрузкой (решение 234), его `play` погасил шлагбаум возврата: странице,
  // видевшей паузу, событие нужно своё. `playing` придёт настоящим — после `loadedmetadata` шлагбаума нет.
  if (!pageSawPlaying && readOriginal(media.paused, ports.element) === false) ports.enqueueEvent("play");
  const realPlay = callOriginal(media.play, ports.element) as Promise<void> | undefined;
  if (realPlay === undefined || typeof realPlay.then !== "function") {
    for (const pending of waiting) pending.resolve();
    return;
  }
  realPlay.then(
    () => {
      for (const pending of waiting) pending.resolve();
    },
    (problem: unknown) => {
      // Браузер не дал играть (политика автозапуска): страница должна узнать, что стоит.
      shadowProblemReporter.report("end: real play() rejected", problem);
      for (const pending of waiting) pending.reject(problem);
      if (pageSawPlaying) ports.enqueueEvent("pause");
    },
  );
}
