// «Человек только что что-то нажал» глазами MAIN-мира (сессия 12a, режим 2 «автозапуск», решения 246, 262).
//
// Режим 2 передаёт видео в плеер, только если запуск начал САМ человек кликом по этому видео (решение 262).
// Страница свой `play()` подделать умеет, а вот доверенное событие ввода (`isTrusted`) — нет. Здесь
// снимаются два следа:
// 1. последнее нажатие мыши (или касание): момент и точка на экране. По нему охрана решает, кликнул ли
//    человек именно по запущенному видео (`ViewerPlayWatch`);
// 2. момент последнего доверенного ввода вообще (клик или клавиша): защита от зацикливания — был ли
//    ввод ПОСЛЕ возврата видео.
// Браузерную «временную активацию» (`navigator.userActivation`, ~5 с после ЛЮБОГО клика или клавиши) мы
// больше не читаем: из-за неё ролик, который лента запускала сама через секунду после лайка, считался
// запуском человека (находка M2 на X, решение 262).
//
// Слушатели ставятся на document_start ОРИГИНАЛЬНЫМ `addEventListener`: страница позже может подменить
// его и «съесть» события. Любой сбой чтения — это «ввода не было»: ошибка наших чтений не должна ни
// долететь до страницы, ни незаметно включить передачу.

import type { ChannelOriginals } from "../page-channel.ts";
import type { OddityReporter } from "../prototype-wrap.ts";

/** Нажатие мыши (касание): когда и где (координаты окна, как у `getBoundingClientRect`). */
export interface PointerPress {
  readonly atMs: number;
  readonly x: number;
  readonly y: number;
}

/** Что остальной MAIN-мир знает про ввод человека: два вопроса, ни одного поля. */
export interface ViewerActivationReader {
  /** Последнее доверенное нажатие основной кнопкой мыши (или касание); `null` — не было. */
  lastPointerPress(): PointerPress | null;
  /** Момент последнего доверенного ввода по часам MAIN-мира; `-Infinity` — ввода не было. */
  lastTrustedInputAtMs(): number;
}

export class ViewerActivation implements ViewerActivationReader {
  readonly #now: () => number;
  #lastTrustedInputAtMs = Number.NEGATIVE_INFINITY;
  #lastPress: PointerPress | null = null;

  constructor(now: () => number) {
    this.#now = now;
  }

  /** Записать момент доверенного ввода. Зовёт только слушатель, поставленный в `installViewerActivation`. */
  noteTrustedInput(): void {
    this.#lastTrustedInputAtMs = this.#now();
  }

  /** Записать нажатие мыши в точке окна (оно же — доверенный ввод). */
  notePointerPress(x: number, y: number): void {
    const atMs = this.#now();
    this.#lastTrustedInputAtMs = atMs;
    this.#lastPress = { atMs, x, y };
  }

  lastPointerPress(): PointerPress | null {
    return this.#lastPress;
  }

  lastTrustedInputAtMs(): number {
    return this.#lastTrustedInputAtMs;
  }
}

/** Ввод человека для защиты от зацикливания (список MDN «Transient activation»). */
interface ActivatingEvent {
  readonly type: string;
  /** Подходит ли именно это событие (клавиша Escape и «мышиный» `pointerup` не активируют). */
  readonly activates: (event: unknown) => boolean;
}

const ACTIVATING_EVENTS: readonly ActivatingEvent[] = [
  { type: "pointerdown", activates: (event) => pointerTypeOf(event) === "mouse" },
  { type: "pointerup", activates: (event) => pointerTypeOf(event) !== "mouse" },
  { type: "mousedown", activates: () => true },
  { type: "touchend", activates: () => true },
  { type: "keydown", activates: (event) => keyOf(event) !== "Escape" },
];

function pointerTypeOf(event: unknown): unknown {
  return (event as { pointerType?: unknown }).pointerType;
}

function keyOf(event: unknown): unknown {
  return (event as { key?: unknown }).key;
}

/** Основная (левая) кнопка мыши; у касания и пера — тоже 0. */
const PRIMARY_BUTTON = 0;

/** Нажатие, которое считается кликом человека: основная кнопка, точка — конечные числа. */
function pressPointOf(event: unknown): { readonly x: number; readonly y: number } | undefined {
  const { button, clientX, clientY } = event as { button?: unknown; clientX?: unknown; clientY?: unknown };
  if (button !== PRIMARY_BUTTON) return undefined;
  if (typeof clientX !== "number" || typeof clientY !== "number" || !Number.isFinite(clientX) || !Number.isFinite(clientY)) return undefined;
  return { x: clientX, y: clientY };
}

/** Типы событий нажатия: `pointerdown` (мышь, касание, перо) и `mousedown` (запасной путь без Pointer Events). */
const PRESS_EVENT_TYPES = ["pointerdown", "mousedown"] as const;

/**
 * Собрать `ViewerActivation` и поставить слушатели на window в захвате ОРИГИНАЛЬНЫМ `addEventListener`
 * (синхронно, на document_start: страница не успеет встать раньше и съесть события). Событие считается
 * вводом человека, только если оно доверенное (`isTrusted` у экземпляра подделать нельзя).
 */
export function installViewerActivation(
  win: object,
  channel: Pick<ChannelOriginals, "addListener">,
  now: () => number,
  reportOddity: OddityReporter,
): ViewerActivation {
  const activation = new ViewerActivation(now);
  const isTrusted = (event: unknown): boolean => (event as { isTrusted?: unknown }).isTrusted === true;
  for (const { type, activates } of ACTIVATING_EVENTS) {
    channel.addListener(
      win,
      type,
      (event) => {
        try {
          if (isTrusted(event) && activates(event)) activation.noteTrustedInput();
        } catch {
          reportOddity("could not read an input event");
        }
      },
      true,
    );
  }
  for (const type of PRESS_EVENT_TYPES) {
    channel.addListener(
      win,
      type,
      (event) => {
        try {
          if (!isTrusted(event)) return;
          const point = pressPointOf(event);
          if (point !== undefined) activation.notePointerPress(point.x, point.y);
        } catch {
          reportOddity("could not read a pointer press");
        }
      },
      true,
    );
  }
  return activation;
}
