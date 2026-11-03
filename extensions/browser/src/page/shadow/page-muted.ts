// `muted` страницы под тенью (находка 00c Ashdi, находка 3 ревью 09b).
//
// Пока тень стоит, настоящий элемент заглушен нами (звук идёт из плеера), а страница видит и
// меняет «свой» `muted` — его хранит этот модуль. Инвариант: на ЛЮБОМ выходе тени (возврат,
// страница забрала видео, страница сменила источник посреди возврата файла, новая передача
// поверх возврата) настоящему элементу возвращается `muted` страницы — ровно один раз. Громкость
// (`volume`) тень не трогает вовсе: её хранит сам настоящий элемент (страница пишет её туда же).

import { type ElementOriginals, readOriginal, writeOriginal } from "./originals.ts";

/** Изменила ли запись страницы её `muted` (тогда страница ждёт `volumechange`). */
export type PageMutedChange = "changed" | "unchanged";

export class PageMutedState {
  readonly #element: HTMLMediaElement;
  readonly #originals: ElementOriginals;
  #pageMuted: boolean;
  #restored = false;

  /** Снимок `muted` страницы — до того, как тень заглушит элемент. */
  constructor(element: HTMLMediaElement, originals: ElementOriginals) {
    this.#element = element;
    this.#originals = originals;
    this.#pageMuted = readOriginal(originals.media.muted, element) === true;
  }

  /** `muted`, как его видит страница. */
  get pageMuted(): boolean {
    return this.#pageMuted;
  }

  /** Страница пишет `muted` под тенью: настоящий элемент остаётся заглушенным. */
  setFromPage(muted: boolean): PageMutedChange {
    if (muted === this.#pageMuted) return "unchanged";
    this.#pageMuted = muted;
    return "changed";
  }

  /** Заглушить настоящий элемент на время тени. */
  muteRealElement(): void {
    writeOriginal(this.#originals.media.muted, this.#element, true);
  }

  /** Вернуть настоящему элементу `muted` страницы. Повторный вызов ничего не делает. */
  restoreToPage(): void {
    if (this.#restored) return;
    this.#restored = true;
    writeOriginal(this.#originals.media.muted, this.#element, this.#pageMuted);
  }
}
