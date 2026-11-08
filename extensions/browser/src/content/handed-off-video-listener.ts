// Якорь переданного видео, сторона изолированного мира (сессия 11a, решение 236; договор —
// src/shared/handed-off-video-contract.ts).
//
// Выбирает случайное имя события, слушает его на своём `window` в фазе захвата и переводит сигналы MAIN-мира
// в два вызова для заглушки: «видео передано (вот элемент)» и «передача ушла». Имя отдаётся вызывающему
// сообщением `watch_handed_off_video` — его надо послать MAIN-миру по личному порту (и только ему).
//
// Слушатель стоит ДО того, как имя ушло в MAIN-мир: так первый же сигнал не потеряется.

import type { PageUiRect } from "../shared/entry-contract.ts";
import {
  createHandedOffVideoEventName,
  WATCH_HANDED_OFF_VIDEO_KIND,
  type RandomBytesFill,
  type WatchHandedOffVideoMessage,
} from "../shared/handed-off-video-contract.ts";

/** Элемент, над которым рисуется заглушка (настоящий `<video>` или хозяин закрытого теневого дерева). */
export interface HandedOffVideoElement {
  readonly isConnected: boolean;
  getBoundingClientRect(): PageUiRect;
}

/** Кто узнаёт о переданном видео (заглушка). */
export interface HandedOffVideoSink {
  videoHandedOff(element: HandedOffVideoElement): void;
  videoLeft(): void;
}

/** Событие сигнала в той мере, в какой оно нужно (настоящее `Event` или фейк теста). */
export interface AnchorSignalEvent {
  readonly target: unknown;
  readonly currentTarget: unknown;
  composedPath?(): readonly unknown[];
  stopImmediatePropagation(): void;
}

/** Окружение изолированного мира. */
export interface HandedOffVideoListenerEnvironment {
  /** Поставить слушатель на своё `window` в фазе захвата. */
  addCaptureListener(eventName: string, listener: (event: AnchorSignalEvent) => void): void;
  /** Криптографический генератор ЭТОГО мира. */
  readonly randomFill: RandomBytesFill;
}

/** Что вернуть вызывающему. */
export interface HandedOffVideoListener {
  /** Отправить MAIN-миру по личному порту (один раз). */
  readonly watchMessage: WatchHandedOffVideoMessage;
  /** Сигналы, цель которых не похожа на элемент (для тестов и диагностики). */
  rejectedSignals(): number;
}

/** Цель похожа на элемент страницы: есть прямоугольник и признак «в документе». */
export function isHandedOffVideoElement(candidate: unknown): candidate is HandedOffVideoElement {
  if (typeof candidate !== "object" || candidate === null) return false;
  const element = candidate as { getBoundingClientRect?: unknown; isConnected?: unknown };
  return typeof element.getBoundingClientRect === "function" && typeof element.isConnected === "boolean";
}

/**
 * Настоящий элемент события. Внутри ОТКРЫТОГО теневого дерева `target` уже перенаправлен на хозяина, а
 * `composedPath()[0]` — сам элемент; закрытые деревья путь скрывает, тогда остаётся хозяин (ограничение договора).
 */
export function signalTargetOf(event: AnchorSignalEvent): unknown {
  const path = typeof event.composedPath === "function" ? event.composedPath() : [];
  return path.length > 0 ? path[0] : event.target;
}

export function listenForHandedOffVideo(environment: HandedOffVideoListenerEnvironment, sink: HandedOffVideoSink): HandedOffVideoListener {
  const eventName = createHandedOffVideoEventName(environment.randomFill);
  let rejected = 0;
  environment.addCaptureListener(eventName, (event) => {
    // Сигнал наш и только наш: странице его не видно ни на одном узле ниже `window`.
    event.stopImmediatePropagation();
    // Разослан на самом `window` — «передача ушла» (цель совпадает с тем, кто слушает).
    if (event.target === event.currentTarget) {
      sink.videoLeft();
      return;
    }
    const target = signalTargetOf(event);
    if (isHandedOffVideoElement(target)) sink.videoHandedOff(target);
    else rejected += 1;
  });
  return { watchMessage: { kind: WATCH_HANDED_OFF_VIDEO_KIND, event_name: eventName }, rejectedSignals: () => rejected };
}
