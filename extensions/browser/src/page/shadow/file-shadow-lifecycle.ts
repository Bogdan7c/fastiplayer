// Жизнь тени файла `<video src>` (сессия 09b, решения 186–187): что тень файла делает иначе, чем
// тень MSE, — на старте, на событиях настоящего элемента, на записи источника страницей и при
// возврате. Вынесено из `shadow-session.ts`: контроллер тени зовёт эти методы одной строкой, а
// всё «файловое» — здесь.
//
// Инварианты:
// - пока тень файла стоит, на настоящем элементе наш пустой MediaStream (решение 186) или — в
//   запасном пути — его собственный источник на паузе;
// - любые доверенные события настоящего элемента гасятся: это загрузка нашего потока или
//   перезагрузка файла при возврате, а не то, что делала страница;
// - любая запись источника страницей (`load()`, `src`, `srcObject`, `setAttribute("src")` →
//   чужое `emptied`) — страница забрала видео: наш поток снимается, её источник работает,
//   тень уходит с `element_released` (как не-MSE источник у тени MSE, 09a2);
// - возврат: источник возвращается, тень остаётся видимой странице, пока элемент заново не
//   загрузит файл и не встанет на T (`FileReturn`), и только потом снимается;
// - смена источника страницей ПОСРЕДИ возврата (через переопределения или мимо них — чужое
//   `emptied`) бросает возврат: ни позиции T, ни `play()` на новом видео страницы (находка 5);
// - новая передача того же элемента посреди возврата (`interruptFileReturn`) сначала заканчивает
//   возврат прежней тени — элемент на T, без игры, — и отдаёт новой тени снимок файла (находка 6).
// `muted` страницы на любом выходе возвращает `detach` контроллера (page-muted.ts).

import type { MediaTimeMicros } from "../../protocol/time.ts";
import type { GateDecision } from "./event-gate.ts";
import { SOURCE_RESET_EVENT_TYPE } from "./event-gate.ts";
import { FileReturn, type FileReturnEnd, type TimeoutScheduler } from "./file-return.ts";
import type { FileSourceMode } from "./file-source-mode.ts";
import { shadowProblemReporter } from "./once-reporter.ts";
import { callOriginal, type ElementOriginals, writeOriginal } from "./originals.ts";
import type { SourceProperty } from "./prototype-overrides.ts";

/** Страница пишет источник элемента. */
export type PageSourceWrite =
  | { readonly kind: "load" }
  | { readonly kind: "set"; readonly property: SourceProperty; readonly value: unknown };

/** Куда и как вернуть элемент странице. */
/** Микросекунд в секунде (позиция T плана — в мкс, `currentTime` — в секундах). */
const MICROS_PER_SECOND = 1_000_000;

export interface ReturnPlan {
  readonly targetUs: MediaTimeMicros;
  readonly resumePlaying: boolean;
  readonly pageSawPlaying: boolean;
}

/** Что жизни тени файла нужно от контроллера тени (он владеет переопределениями и шлагбаумом). */
export interface FileShadowHost {
  readonly element: HTMLMediaElement;
  readonly originals: ElementOriginals;
  readonly scheduleTimeout: TimeoutScheduler;
  /** Тень ещё исполняет команды плеера (не снята и возврат не начался). */
  isActive(): boolean;
  /** Возврат начался: команды плеера больше не исполняются, переопределения пока стоят. */
  stopCommands(): void;
  /** Страница забрала видео: снять тень, `handoff_ended{element_released}`. */
  releaseToPage(): void;
  /** Снять переопределения и шлагбаум с элемента. */
  detach(): void;
  /** Сообщить наружу `handoff_ended{returned}`. */
  reportReturned(): void;
  /** Вернуть элементу громкость, скорость, позицию и игру/паузу (путь возврата MSE). */
  returnRealElement(plan: ReturnPlan): void;
  /** Источник уже страницы: отдать ей ожидающие `play()` (отказом) и колбэки кадров. */
  handOverWithoutPosition(): void;
  /** Внутренняя ошибка не долетает до страницы: одна строка отладки. */
  attempt(context: string, action: () => void): void;
}

/** Как закончился возврат файла (итог `FileReturn` или решение снаружи). */
type ReturnCompletion =
  | FileReturnEnd
  /** Страница сама сменила источник посреди возврата. */
  | "page_took_over"
  /** Новая передача этого же элемента посреди возврата. */
  | "superseded";

/**
 * Тени файла, чей возврат ещё идёт, по элементу. Нужны новой передаче того же элемента: тень
 * прежней уже доложила `returned`, но с элемента ещё не снята (`interruptFileReturn`).
 */
const returningFiles = new WeakMap<object, FileShadowLifecycle>();

/**
 * Новая передача элемента: если у него идёт возврат файла прежней тени — закончить его сейчас
 * (элемент на T, без игры, `muted` страницы) и отдать снимок файла новой тени. `null` — возврата
 * не было, элемент свободен или под обычной тенью.
 */
export function interruptFileReturn(element: object): FileSourceMode | null {
  return returningFiles.get(element)?.interruptReturn() ?? null;
}

export class FileShadowLifecycle {
  readonly #mode: FileSourceMode;
  readonly #host: FileShadowHost;
  /** Идёт возврат: элемент заново загружает файл (`null` — не идёт). */
  #fileReturn: FileReturn | null = null;
  #plan: ReturnPlan | null = null;

  constructor(mode: FileSourceMode, host: FileShadowHost) {
    this.#mode = mode;
    this.#host = host;
  }

  /** Страница уже сменила атрибут `src`, хотя `emptied` ещё в пути (file-source-mode.ts). */
  get pageRewroteSource(): boolean {
    return this.#mode.pageRewroteSourceAttribute();
  }

  /** Снимок файла — то, что видит страница. */
  get view(): FileSourceMode {
    return this.#mode;
  }

  /** Старт: шлагбаум уже стоит, снимаем файл с настоящего элемента (решение 186). */
  start(): void {
    this.#host.attempt("start: stop the file download", () => this.#mode.stopRealDownload());
  }

  /** Доверенное событие настоящего элемента: все гасятся (см. инварианты в шапке). */
  onTrustedEvent(type: string): GateDecision {
    const host = this.#host;
    if (type === SOURCE_RESET_EVENT_TYPE && this.#mode.classifyEmptied() === "page_source_change") {
      // Страница сменила источник мимо наших переопределений (`setAttribute("src")`, `<source>` и
      // `load()` из другого фрейма).
      if (this.#fileReturn !== null) host.attempt("file return: page changed the source", () => this.#abandonReturn());
      else if (host.isActive()) host.attempt("file: page changed the source", () => host.releaseToPage());
      return "suppress";
    }
    const fileReturn = this.#fileReturn;
    if (fileReturn !== null) {
      host.attempt("file return", () => fileReturn.onRealEvent(type));
    } else if (type === "play") {
      // Кто-то запустил настоящий элемент мимо тени (оригинал из другого фрейма) — снова пауза.
      host.attempt("re-pause real element", () => callOriginal(host.originals.media.pause, host.element));
    }
    return "suppress";
  }

  /**
   * Запись источника страницей. Ошибка самой записи (неверный тип `srcObject`) уходит странице,
   * как у браузера, и тень остаётся.
   */
  pageWritesSource(write: PageSourceWrite): void {
    const { element, originals } = this.#host;
    const media = originals.media;
    if (write.kind === "set") {
      const accessor = write.property === "src" ? media.src : media.srcObject;
      if (accessor !== undefined) writeOriginal(accessor, element, write.value);
      if (write.property === "srcObject") this.#mode.notePageReplacedSrcObject();
    } else if (!this.#mode.ownsRealSource) {
      // Нашего потока на элементе нет (запасной путь или возврат уже вернул источник): `load()` —
      // как без тени. При нашем потоке «алгоритм загрузки» исполнит его снятие в `beforeRelease`.
      callOriginal(media.load, element);
    }
    if (this.#fileReturn !== null) {
      // Возврат ещё идёт, а страница уже сама сменила источник: ждать нечего.
      this.#abandonReturn();
      return;
    }
    if (this.#host.isActive()) this.#host.releaseToPage();
  }

  /**
   * Новая передача этого элемента посреди возврата (`interruptFileReturn`): возврат кончается
   * сейчас. `null` — возврат не идёт (прерывать нечего).
   */
  interruptReturn(): FileSourceMode | null {
    const fileReturn = this.#fileReturn;
    if (fileReturn === null) return null;
    fileReturn.abandon();
    this.#complete("superseded");
    return this.#mode;
  }

  /** Страница сменила источник посреди возврата: возврат брошен, видео — её. */
  #abandonReturn(): void {
    this.#fileReturn?.abandon();
    this.#complete("page_took_over");
  }

  /** Тень уходит по решению страницы: снять наш поток, иначе её источник (младше `srcObject`) не заработает. */
  beforeRelease(): void {
    this.#host.attempt("release: restore the file source", () => this.#mode.restoreRealSource());
  }

  /** Возврат по команде плеера (решение 186). Плеер узнаёт о конце сразу. */
  end(plan: ReturnPlan): void {
    const host = this.#host;
    host.stopCommands();
    this.#plan = plan;
    let restored = false;
    host.attempt("end: return the file to the element", () => {
      restored = this.#mode.returnFileToElement() !== "nothing_to_restore";
    });
    if (restored) {
      returningFiles.set(host.element, this);
      this.#fileReturn = new FileReturn({
        element: host.element,
        originals: host.originals,
        targetUs: plan.targetUs,
        scheduleTimeout: host.scheduleTimeout,
        onFinished: (end) => this.#complete(end),
      });
    } else {
      // Источник не снимался (запасной путь) — элемент и так с файлом: возврат как у MSE, сразу.
      this.#complete("restored");
    }
    host.reportReturned();
  }

  /**
   * Конец возврата: тень снимается (`detach` возвращает и `muted` страницы), элемент — на T на
   * паузе или в игре; при смене источника страницей — как есть; при новой передаче — на T без игры.
   */
  #complete(end: ReturnCompletion): void {
    const host = this.#host;
    this.#fileReturn = null;
    if (returningFiles.get(host.element) === this) returningFiles.delete(host.element);
    if (end === "load_failed" || end === "timed_out") shadowProblemReporter.report(`shadow: file return ${end}`, undefined);
    host.detach();
    const plan = this.#plan;
    if (plan === null) return;
    switch (end) {
      case "page_took_over":
        host.handOverWithoutPosition();
        return;
      case "superseded":
        // Новая тень сейчас встанет на этот элемент и прочтёт его позицию: она должна быть T (до
        // метаданных — позиция начала воспроизведения). Играть незачем: новая тень всё равно
        // ставит элемент на паузу, а играть будет плеер.
        host.attempt("superseded: place the real element at T", () =>
          writeOriginal(host.originals.media.currentTime, host.element, plan.targetUs / MICROS_PER_SECOND),
        );
        host.handOverWithoutPosition();
        return;
      case "restored":
      case "start_position_set":
      case "load_failed":
      case "timed_out":
        host.attempt("end: restore real element", () => host.returnRealElement(plan));
        return;
    }
  }
}
