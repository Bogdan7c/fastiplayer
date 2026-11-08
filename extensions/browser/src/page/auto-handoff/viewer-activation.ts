// «Человек только что что-то нажал» глазами MAIN-мира (сессия 12a, режим 2 «автозапуск», решение 246).
//
// Режим 2 передаёт видео в плеер, только если запуск начал САМ человек. Страница свой `play()` подделать
// умеет, а вот «настоящий жест» — нет. Берём два независимых источника:
// 1. браузерное понятие «временная активация» (`navigator.userActivation.isActive`, около 5 с после клика
//    или клавиши): это то самое правило, по которому браузер сам разрешает звучащий запуск;
// 2. собственные отметки времени доверенных событий ввода (`isTrusted`): нужны для защиты от зацикливания
//    (был ли ввод ПОСЛЕ возврата видео) и как запасной путь там, где `userActivation` нет.
//
// Всё снимается на document_start ОРИГИНАЛАМИ: страница позже может подменить геттер активации или
// `addEventListener`, и тогда она обманула бы нас («активация есть всегда» или «ввода не было»). Любой сбой —
// это «активации нет»: ошибка наших чтений не должна ни долететь до страницы, ни незаметно включить передачу.

import type { ChannelOriginals } from "../page-channel.ts";
import type { OddityReporter } from "../prototype-wrap.ts";

/**
 * Сколько держится «свой» след ввода там, где браузерной активации нет (запасной путь). Совпадает с окном
 * временной активации Chromium и Firefox (около 5 с), чтобы поведение не зависело от браузера.
 */
export const FALLBACK_ACTIVATION_WINDOW_MS = 5_000;

/** Что остальной MAIN-мир знает про ввод человека: два вопроса, ни одного поля. */
export interface ViewerActivationReader {
  /** Браузер считает, что жест человека «свежий» (или, без `userActivation`, свой ввод моложе окна). */
  hasTransientActivation(): boolean;
  /** Момент последнего доверенного ввода по часам MAIN-мира; `-Infinity` — ввода не было. */
  lastTrustedInputAtMs(): number;
}

/** Окно, с которого снимаются оригиналы (в тестах — фейк; всё необязательно: нет — значит «нет API»). */
export interface ActivationWindow {
  readonly navigator?: object;
  readonly Navigator?: { readonly prototype: object };
  readonly UserActivation?: { readonly prototype: object };
}

/** Чтение браузерной активации; `undefined` — API нет (старый браузер, фейк теста). */
export type TransientActivationReader = () => boolean | undefined;

/** Что `ViewerActivation` нужно снаружи. */
export interface ViewerActivationEnvironment {
  readonly readTransientActivation: TransientActivationReader;
  /** Монотонные часы окна, снятые на document_start, мс. */
  readonly now: () => number;
  readonly reportOddity: OddityReporter;
}

export class ViewerActivation implements ViewerActivationReader {
  readonly #environment: ViewerActivationEnvironment;
  #lastTrustedInputAtMs = Number.NEGATIVE_INFINITY;
  #readFailureReported = false;

  constructor(environment: ViewerActivationEnvironment) {
    this.#environment = environment;
  }

  /** Записать момент доверенного ввода. Зовёт только слушатель, поставленный в `installViewerActivation`. */
  noteTrustedInput(): void {
    this.#lastTrustedInputAtMs = this.#environment.now();
  }

  hasTransientActivation(): boolean {
    try {
      const fromBrowser = this.#environment.readTransientActivation();
      if (fromBrowser !== undefined) return fromBrowser;
    } catch {
      // Чтение сломала страница или браузер: не гадаем в пользу передачи, но один раз оставляем след.
      if (!this.#readFailureReported) {
        this.#readFailureReported = true;
        this.#environment.reportOddity("could not read navigator.userActivation");
      }
      return false;
    }
    // `userActivation` нет — запасной путь по своим отметкам.
    return this.#environment.now() - this.#lastTrustedInputAtMs <= FALLBACK_ACTIVATION_WINDOW_MS;
  }

  lastTrustedInputAtMs(): number {
    return this.#lastTrustedInputAtMs;
  }
}

/** Ввод, после которого браузер считает жест «активирующим» (список MDN «Transient activation»). */
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

/** Достать геттер свойства из прототипа (своё или унаследованное); `undefined` — нет. */
function findGetter(prototype: object, name: string): ((this: unknown) => unknown) | undefined {
  let current: object | null = prototype;
  while (current !== null) {
    const getter = Object.getOwnPropertyDescriptor(current, name)?.get;
    if (getter !== undefined) return getter as (this: unknown) => unknown;
    current = Object.getPrototypeOf(current) as object | null;
  }
  return undefined;
}

/**
 * Читатель браузерной активации на оригинальных геттерах (`Navigator.prototype.userActivation` →
 * `UserActivation.prototype.isActive`). Нет любого звена — читатель всегда отвечает `undefined` («API нет»).
 */
export function captureTransientActivationReader(win: ActivationWindow): TransientActivationReader {
  const activationGetter = win.Navigator === undefined ? undefined : findGetter(win.Navigator.prototype, "userActivation");
  const isActiveGetter = win.UserActivation === undefined ? undefined : findGetter(win.UserActivation.prototype, "isActive");
  const navigatorObject = win.navigator;
  if (activationGetter === undefined || isActiveGetter === undefined || navigatorObject === undefined) return () => undefined;
  return () => {
    const activation: unknown = Reflect.apply(activationGetter, navigatorObject, []);
    if (typeof activation !== "object" || activation === null) return undefined;
    return Reflect.apply(isActiveGetter, activation, []) === true;
  };
}

/**
 * Собрать `ViewerActivation` и поставить слушатели на window в захвате ОРИГИНАЛЬНЫМ `addEventListener`
 * (синхронно, на document_start: страница не успеет встать раньше и съесть события). Событие считается
 * вводом человека, только если оно доверенное (`isTrusted` у экземпляра подделать нельзя).
 */
export function installViewerActivation(
  win: ActivationWindow & object,
  channel: Pick<ChannelOriginals, "addListener">,
  now: () => number,
  reportOddity: OddityReporter,
): ViewerActivation {
  const activation = new ViewerActivation({ readTransientActivation: captureTransientActivationReader(win), now, reportOddity });
  for (const { type, activates } of ACTIVATING_EVENTS) {
    channel.addListener(
      win,
      type,
      (event) => {
        try {
          if ((event as { isTrusted?: unknown }).isTrusted === true && activates(event)) activation.noteTrustedInput();
        } catch {
          reportOddity("could not read an input event");
        }
      },
      true,
    );
  }
  return activation;
}
