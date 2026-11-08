// Охрана режима 2 «автозапуск» (сессия 12a, решения 246–254): «видео запустил САМ человек, и со звуком»
// (звук можно не требовать — фильтр звука выключается в настройках, решение 260).
//
// Владеет политикой "кто считается зрителем" и больше ничем: включена ли охрана, кредит клика, метки
// `play()` и метки «передача этого элемента кончилась». Правил сайтов, стоп-листа и порога длины здесь нет
// и быть не должно: страница делит мир с MAIN-скриптом, и всё, что здесь лежит, она может прочитать. Сюда
// попадает только то, что страница и так знает (свои клики, свой `play()`); решает фон.
//
// Правило строгое (решение владельца): запуск зрителя — это либо свежая временная активация браузера, либо
// действующий «кредит клика» от фона (унаследованный клик через короткую рекламу). Защита от зацикливания:
// пока передача элемента не кончилась, ничего не меняется, а после конца (Return, окно закрыто, замена)
// новый запуск того же элемента считается только при НОВОМ доверенном вводе позже конца. Сам клик «Return»
// стоит РАНЬШЕ конца передачи (его ответ идёт кругом через фон), поэтому не засчитывается.
//
// Сторож (`next-video-guard.ts`) зовёт три «судьи» в три момента, и каждый возвращает ответ из ОДНОГО набора:
// - `judgePlayCall` — страница зовёт `play()`; при успехе запоминается метка (видео ещё может ждать метаданных);
// - `judgeTrustedPlayEvent` — доверенное событие `play` (родные кнопки, атрибут `autoplay`);
// - `judgeLoadedMetadata` — метаданные пришли; годится, только если недавно был засчитанный `play()`.
// `seeked` зрителя ничего не начинает вовсе: этого судьи нет.

import type { AutoHandoffWatchState } from "../../shared/auto-handoff-contract.ts";
import { MAX_PLAY_CREDIT_LEASE_MS } from "../../shared/auto-handoff-contract.ts";
import type { ViewerActivationReader } from "./viewer-activation.ts";

/**
 * Сколько живёт метка засчитанного `play()`, мс. Между `play()` и метаданными ролика проходят сеть и
 * сервер сайта: 15 с покрывает медленный старт, но не позволяет «вчерашнему» клику запустить передачу.
 */
export const PLAY_CALL_MARK_WINDOW_MS = 15_000;

/** Слышно ли звук у настоящего элемента (читается оригинальными геттерами: `muted`, `volume`). */
export type ElementAudibility = "audible" | "silent";

/** Ответ судьи: `viewer_started` — запустил зритель; остальное — почему нет (для тестов и диагностики). */
export type ViewerPlayVerdict =
  | "viewer_started"
  | "watch_off"
  | "muted"
  | "handed_back_without_new_input"
  | "no_viewer_gesture"
  | "no_credited_play_call";

/** Простые факты момента запуска: на них судья не обращается ни к часам, ни к браузеру. */
export interface ViewerPlayFacts {
  /** Состояние охраны: выключена, только со звуком (`on`) или и без звука (`on_including_muted`). */
  readonly watch: AutoHandoffWatchState;
  /** Браузерная временная активация жива. */
  readonly transientActivation: boolean;
  /** Кредит клика от фона действует. */
  readonly creditActive: boolean;
  /** Когда кончилась передача этого элемента; `null` — не передавался. */
  readonly handedBackAtMs: number | null;
  /** Момент последнего доверенного ввода; `-Infinity` — не было. */
  readonly lastTrustedInputAtMs: number;
  readonly audibility: ElementAudibility;
}

/**
 * Чистое решение «запустил зритель?». Порядок важен: выключенная охрана и заглушенное видео отсекаются
 * первыми; кредит НЕ отменяет метку возврата (возвращённому видео нужен свежий ввод после метки).
 */
export function judgeViewerPlay(facts: ViewerPlayFacts): ViewerPlayVerdict {
  if (facts.watch === "off") return "watch_off";
  // Фильтр звука снят (решение 260) — беззвучное видео судится дальше, как звучащее: жест всё равно нужен.
  if (facts.audibility === "silent" && facts.watch !== "on_including_muted") return "muted";
  if (facts.handedBackAtMs !== null && facts.lastTrustedInputAtMs <= facts.handedBackAtMs) return "handed_back_without_new_input";
  if (!facts.transientActivation && !facts.creditActive) return "no_viewer_gesture";
  return "viewer_started";
}

export interface ViewerPlayWatchEnvironment {
  readonly activation: ViewerActivationReader;
  /** Монотонные часы окна, мс. */
  readonly now: () => number;
}

export class ViewerPlayWatch {
  readonly #environment: ViewerPlayWatchEnvironment;
  /** Время `play()`, который зритель имел право начать (по нему принимаем `loadedmetadata`). */
  readonly #creditedPlayCalls = new WeakMap<object, number>();
  /** Время конца последней передачи элемента. */
  readonly #handedBack = new WeakMap<object, number>();
  #watchState: AutoHandoffWatchState = "off";
  #creditUntilMs = Number.NEGATIVE_INFINITY;

  constructor(environment: ViewerPlayWatchEnvironment) {
    this.#environment = environment;
  }

  /** Изолированный мир сообщил: охрана включена или выключена. Выключена по умолчанию (решение 9). */
  setWatch(state: AutoHandoffWatchState): void {
    this.#watchState = state;
  }

  /** Охрана включена (со звуком или без — неважно). */
  isWatchOn(): boolean {
    return this.#watchState !== "off";
  }

  /** Фон выдал «кредит клика» на срок; новый кредит заменяет старый (цепочка реклам продлевает его). */
  grantCredit(leaseMs: number): void {
    // Страховка от мусора: срок вне договорного диапазона не выдаёт кредита (команда уже проверена выше).
    if (!Number.isFinite(leaseMs) || leaseMs <= 0) return;
    this.#creditUntilMs = this.#environment.now() + Math.min(leaseMs, MAX_PLAY_CREDIT_LEASE_MS);
  }

  hasActiveCredit(): boolean {
    return this.#environment.now() < this.#creditUntilMs;
  }

  /** Передача элемента кончилась (Return, окно закрыто, замена): дальше запуск засчитывается только после нового ввода. */
  noteHandoffEnded(element: object): void {
    this.#handedBack.set(element, this.#environment.now());
  }

  /** Страница зовёт `play()`. Засчитан — запоминается метка, чтобы принять `loadedmetadata` позже. */
  judgePlayCall(element: object, audibility: ElementAudibility): ViewerPlayVerdict {
    const verdict = this.#judgeNow(element, audibility);
    if (verdict === "viewer_started") this.#creditedPlayCalls.set(element, this.#environment.now());
    return verdict;
  }

  /** Доверенное событие `play` (родные кнопки, `autoplay`): судим по обстановке сейчас. */
  judgeTrustedPlayEvent(element: object, audibility: ElementAudibility): ViewerPlayVerdict {
    return this.#judgeNow(element, audibility);
  }

  /**
   * Метаданные пришли. Годится, только если недавно был засчитанный `play()` этого элемента: сама активация
   * к этому времени могла истечь (метаданные идут секундами), а сам по себе `loadedmetadata` ничего не значит.
   */
  judgeLoadedMetadata(element: object, audibility: ElementAudibility): ViewerPlayVerdict {
    const markedAtMs = this.#creditedPlayCalls.get(element);
    const now = this.#environment.now();
    if (markedAtMs === undefined || now - markedAtMs > PLAY_CALL_MARK_WINDOW_MS) return "no_credited_play_call";
    const handedBackAtMs = this.#handedBack.get(element) ?? null;
    // Передача кончилась после засчитанного `play()`: тот запуск принадлежал уже закрытой передаче.
    if (handedBackAtMs !== null && handedBackAtMs >= markedAtMs) return "handed_back_without_new_input";
    const verdict = judgeViewerPlay({
      watch: this.#watchState,
      // Жест доказан в момент `play()` — метка и есть его след.
      transientActivation: true,
      creditActive: false,
      handedBackAtMs: null,
      lastTrustedInputAtMs: this.#environment.activation.lastTrustedInputAtMs(),
      audibility,
    });
    if (verdict === "viewer_started") this.#creditedPlayCalls.delete(element);
    return verdict;
  }

  #judgeNow(element: object, audibility: ElementAudibility): ViewerPlayVerdict {
    return judgeViewerPlay({
      watch: this.#watchState,
      transientActivation: this.#environment.activation.hasTransientActivation(),
      creditActive: this.hasActiveCredit(),
      handedBackAtMs: this.#handedBack.get(element) ?? null,
      lastTrustedInputAtMs: this.#environment.activation.lastTrustedInputAtMs(),
      audibility,
    });
  }
}
