// Шлагбаум настоящих медиасобытий переданного элемента.
//
// Настоящий элемент в тени стоит на паузе, а мы его ещё и двигаем (пауза на старте, заглушка,
// подтягивание позиции). Его собственные `pause`/`seeking`/`timeupdate`/… противоречили бы тому,
// что показывает тень, поэтому они гасятся в фазе захвата раньше слушателей страницы.
//
// Как: слушатель захвата на `window` (он первый на пути события для элемента документа) и,
// если элемент внутри shadow root, ещё и на этом корне — медиасобытия не `composed`, до `window`
// из теневого дерева они не доходят. Гасим только доверенные (`isTrusted`) события именно этого
// элемента; свои синтетические события тени (`isTrusted=false`) проходят.

import { callOriginal, type ElementOriginals } from "./originals.ts";

/**
 * События, которые отражают ВОСПРОИЗВЕДЕНИЕ (их владелец — тень): настоящие гасятся всегда.
 * Не гасятся события, отражающие ПОТОК и элемент (`durationchange`, `loadedmetadata`,
 * `loadeddata`, `resize`, `progress`, `error`, `encrypted`, `waitingforkey`, `emptied`,
 * `loadstart`, `abort`, `suspend`, `stalled`, …): поток и элемент общие с браузером, плеер
 * страницы должен видеть их как есть (например, `durationchange` после `endOfStream`).
 */
export const PLAYBACK_EVENT_TYPES: readonly string[] = [
  "play",
  "playing",
  "pause",
  "timeupdate",
  "seeking",
  "seeked",
  "waiting",
  "ended",
  "ratechange",
  "volumechange",
];

/**
 * Готовность данных (`canplay`, `canplaythrough`) пропускается: после смены источника плеер
 * страницы может ждать её, прежде чем звать `play()`. Гасится только та, что вызвана нашей
 * перестановкой настоящего элемента (решает контроллер).
 */
export const READINESS_EVENT_TYPES: readonly string[] = ["canplay", "canplaythrough"];

/** Событие смены источника: тень решает, снять ли себя, и всегда пропускает его странице. */
export const SOURCE_RESET_EVENT_TYPE = "emptied";

/** Решение по одному доверенному событию настоящего элемента. */
export type GateDecision = "suppress" | "pass";

export type TrustedEventHandler = (type: string) => GateDecision;

/** Снять шлагбаум (все слушатели). */
export type GateDisposer = () => void;

/**
 * Поставить шлагбаум на элемент. `onTrustedEvent` зовётся для каждого доверенного события
 * элемента из наблюдаемого списка; ответ `suppress` останавливает событие для страницы.
 */
export function installEventGate(
  element: HTMLMediaElement,
  win: Window,
  originals: ElementOriginals,
  onTrustedEvent: TrustedEventHandler,
): GateDisposer {
  const events = originals.events;
  const listener = (event: Event): void => {
    // `isTrusted` — собственное неподделываемое свойство события: подмена прототипа его не трогает.
    if (!event.isTrusted) return;
    if (callOriginal(events.eventTarget, event) !== element) return;
    if (onTrustedEvent(event.type) === "suppress") callOriginal(events.stopImmediatePropagation, event);
  };
  const watchedTypes = [...PLAYBACK_EVENT_TYPES, ...READINESS_EVENT_TYPES, SOURCE_RESET_EVENT_TYPE];
  const listenRoots = gateRoots(element, win, originals);
  const dispose = (): void => {
    for (const root of listenRoots) {
      for (const type of watchedTypes) {
        try {
          callOriginal(events.removeEventListener, root, [type, listener, true]);
        } catch {
          // Не поставленный слушатель снимать нечего: откат после сбоя идёт по всем.
        }
      }
    }
  };
  try {
    for (const root of listenRoots) {
      for (const type of watchedTypes) callOriginal(events.addEventListener, root, [type, listener, true]);
    }
  } catch (problem) {
    // Шлагбаум встал наполовину — снимаем всё, что успели, и сообщаем о сбое выше.
    dispose();
    throw problem;
  }
  return dispose;
}

/** Где ставить слушатели захвата: `window` и корень элемента, если это не документ. */
function gateRoots(element: HTMLMediaElement, win: Window, originals: ElementOriginals): EventTarget[] {
  const roots: EventTarget[] = [win];
  const root = callOriginal(originals.events.getRootNode, element) as EventTarget;
  // Корень-документ лежит на пути к `window` — второй слушатель там не нужен. Теневой корень
  // или отсоединённое поддерево (корень — верхний предок) событие к `window` не пропустит.
  if (root !== (win as unknown as { readonly document: unknown }).document) roots.push(root);
  return roots;
}
