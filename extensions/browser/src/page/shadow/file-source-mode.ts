// Тень файла `<video src>` (сессия 09b, решения 186–187): настоящий элемент перестаёт качать и
// декодировать файл, а страница видит файл таким, каким он был в момент передачи.
//
// Владеет:
// - снимком файла ДО снятия источника: длительность, размер кадра, `currentSrc`, «весь файл»
//   для `buffered`/`seekable` (whole-file-ranges.ts);
// - знанием «на настоящем элементе стоит НАШ пустой MediaStream» и счётом доверенных `emptied`,
//   которые вызвали наши собственные записи (их страница видеть не должна, и они не значат, что
//   страница сменила источник).
//
// Как снимается источник: `srcObject = new MediaStream()` оригинальным сеттером. `srcObject`
// главнее атрибута `src` и `<source>`, поэтому DOM страницы не меняется (атрибуты на месте), а
// браузер по «алгоритму загрузки» бросает загрузку файла (событие `abort`) и играть ему нечего.
// Возврат — `srcObject = null` тем же сеттером: элемент снова загружает свой файл.
// Файл, пришедший мимо разметки (видео-документ Firefox: прямая ссылка на `.mp4` грузится каналом
// самого документа, у `<video>` нет ни `src`, ни `<source>`), после `srcObject = null` грузить
// нечего — элемент остаётся пустым (дефект 4 живой проверки 09b). Такому элементу при возврате
// адрес файла из снимка ставится атрибутом `src` (оригинальным сеттером): это единственный способ
// снова дать ему тот же файл. При уходе по решению страницы так не делается — у неё свой источник.
// Нет MediaStream в браузере или запись не удалась — источник остаётся (честный запасной путь:
// настоящий элемент на паузе, как у MSE; браузер докачает своё окно предзагрузки и встанет), со
// следом в отладке.

import { callOriginal, type ElementOriginals, type OriginalAccessor, readOriginal, writeOriginal } from "./originals.ts";
import { shadowProblemReporter } from "./once-reporter.ts";
import { hasSourceDeclaredInMarkup } from "./source-reset-policy.ts";
import { captureWholeFileRanges, type TimeRangesShape } from "./whole-file-ranges.ts";

/**
 * Что тень показывает странице вместо настоящего источника (переопределения прототипов
 * спрашивают это у тени; `null` у тени MSE — тогда отвечает настоящий элемент).
 */
export interface ShadowSourceView {
  duration(): number;
  buffered(): TimeRangesShape;
  seekable(): TimeRangesShape;
  networkState(): number;
  currentSrc(): string;
  /** Что страница сама поставила в `srcObject` (у файла — ничего). */
  srcObject(): unknown;
  videoWidth(): number;
  videoHeight(): number;
}

/** Удалось ли снять файл с настоящего элемента. */
export type RealSourceStop = "stopped" | "kept_source";

/**
 * Откуда настоящий элемент взял файл (снимок до снятия источника):
 * - `markup` — атрибут `src` или дочерний `<source>`: после `srcObject = null` браузер сам
 *   выберет этот источник заново;
 * - `outside_markup` — мимо DOM (канал видео-документа Firefox): вернуть файл можно только адресом.
 */
export type FileSourceDeclaration = "markup" | "outside_markup";

/** Чем кончилась попытка вернуть файл настоящему элементу. */
export type FileRestore =
  /** Наш поток снят, элемент сам загрузит свой источник из разметки. */
  | "restored"
  /** Наш поток снят, элементу дан адрес файла из снимка (файл был мимо разметки). */
  | "restored_with_address"
  /** Нашего потока на элементе нет — возвращать нечего. */
  | "nothing_to_restore";

/** Чьё это доверенное `emptied`. */
export type EmptiedOrigin = "own_write" | "page_source_change";

/**
 * `HTMLMediaElement.NETWORK_IDLE`: «источник выбран, сеть сейчас не нужна» — честный ответ для
 * файла, который страница считает загруженным целиком (решение 187).
 */
const NETWORK_IDLE = 1;
/** `HTMLMediaElement.NETWORK_EMPTY`: без источника «алгоритм загрузки» `emptied` не шлёт. */
const NETWORK_EMPTY = 0;

/** Снимок файла, каким его видит страница, и счёт наших `emptied` в пути. */
interface FileSnapshot {
  readonly durationSeconds: number;
  readonly videoWidth: number;
  readonly videoHeight: number;
  readonly currentSrc: string;
  readonly declaration: FileSourceDeclaration;
  readonly ranges: () => TimeRangesShape;
  readonly ownEmptiedPending: number;
}

export class FileSourceMode implements ShadowSourceView {
  readonly #element: HTMLMediaElement;
  readonly #originals: ElementOriginals;
  readonly #durationSeconds: number;
  readonly #videoWidth: number;
  readonly #videoHeight: number;
  readonly #currentSrc: string;
  readonly #declaration: FileSourceDeclaration;
  readonly #ranges: () => TimeRangesShape;
  /** На настоящем элементе стоит наш пустой MediaStream. */
  #ownsRealSource = false;
  /** Сколько доверенных `emptied` ещё придёт от наших собственных записей источника. */
  #ownEmptiedPending = 0;

  /** Снимок файла с элемента — до любых наших действий с источником. */
  static capture(element: HTMLMediaElement, originals: ElementOriginals): FileSourceMode {
    const media = originals.media;
    const video = originals.video;
    const durationSeconds = Number(readOriginal(media.duration, element));
    return new FileSourceMode(element, originals, {
      durationSeconds,
      videoWidth: video.videoWidth === undefined ? 0 : Number(readOriginal(video.videoWidth, element)),
      videoHeight: video.videoHeight === undefined ? 0 : Number(readOriginal(video.videoHeight, element)),
      currentSrc: media.currentSrc === undefined ? "" : String(readOriginal(media.currentSrc, element)),
      declaration: hasSourceDeclaredInMarkup(element, originals) ? "markup" : "outside_markup",
      ranges: captureWholeFileRanges(element, originals, durationSeconds),
      ownEmptiedPending: 0,
    });
  }

  /**
   * Новая тень поверх прерванного возврата прежней (находка 6 ревью 09b): настоящий элемент ещё
   * перезагружает файл (метаданных может не быть), поэтому снимок берётся у прежней тени — это тот
   * же файл, каким его видела страница. Ей же принадлежат ещё не пришедшие доверенные `emptied` от
   * нашей записи «вернуть источник»: они переходят в счёт новой тени, иначе новая тень приняла бы
   * их за «страница сменила источник».
   */
  static inheriting(element: HTMLMediaElement, originals: ElementOriginals, previous: FileSourceMode): FileSourceMode {
    return new FileSourceMode(element, originals, {
      durationSeconds: previous.#durationSeconds,
      videoWidth: previous.#videoWidth,
      videoHeight: previous.#videoHeight,
      currentSrc: previous.#currentSrc,
      declaration: previous.#declaration,
      ranges: previous.#ranges,
      ownEmptiedPending: previous.#ownEmptiedPending,
    });
  }

  private constructor(element: HTMLMediaElement, originals: ElementOriginals, snapshot: FileSnapshot) {
    this.#element = element;
    this.#originals = originals;
    this.#durationSeconds = snapshot.durationSeconds;
    this.#videoWidth = snapshot.videoWidth;
    this.#videoHeight = snapshot.videoHeight;
    this.#currentSrc = snapshot.currentSrc;
    this.#declaration = snapshot.declaration;
    this.#ranges = snapshot.ranges;
    this.#ownEmptiedPending = snapshot.ownEmptiedPending;
  }

  /** Длительность файла из снимка, мкс (`undefined` — метаданных не было). */
  get durationUs(): number | undefined {
    const seconds = this.#durationSeconds;
    return Number.isFinite(seconds) && seconds >= 0 ? Math.floor(seconds * 1_000_000) : undefined;
  }

  /** Стоит ли на элементе наш MediaStream (иначе возвращать нечего). */
  get ownsRealSource(): boolean {
    return this.#ownsRealSource;
  }

  /**
   * Снять файл с настоящего элемента (решение 186). Зовётся, когда шлагбаум тени уже стоит: события
   * этой записи (`abort`, `emptied`, `loadstart`, …) странице не достанутся.
   */
  stopRealDownload(): RealSourceStop {
    const accessor = this.#originals.media.srcObject;
    const MediaStreamConstructor = this.#originals.MediaStreamConstructor;
    if (accessor === undefined || MediaStreamConstructor === undefined) {
      shadowProblemReporter.report("file shadow: no MediaStream, the real element keeps its source", undefined);
      return "kept_source";
    }
    try {
      this.#writeOwnSource(accessor, new MediaStreamConstructor());
    } catch (problem) {
      shadowProblemReporter.report("file shadow: could not stop the real download", problem);
      return "kept_source";
    }
    this.#ownsRealSource = true;
    return "stopped";
  }

  /**
   * Снять наш поток (`srcObject = null`), когда видео забирает страница: дальше работает её
   * собственный источник. Адрес из снимка здесь НЕ ставится — страница уже решила, что играть.
   * Если на элементе уже не наш поток — ничего.
   */
  restoreRealSource(): "restored" | "nothing_to_restore" {
    const accessor = this.#originals.media.srcObject;
    if (!this.#ownsRealSource || accessor === undefined) return "nothing_to_restore";
    this.#ownsRealSource = false;
    this.#writeOwnSource(accessor, null);
    return "restored";
  }

  /**
   * Вернуть элементу ТОТ ЖЕ файл по команде плеера (возврат на T). Снимает наш поток; файлу из
   * разметки этого достаточно, файлу мимо разметки (видео-документ) ставится адрес из снимка.
   */
  returnFileToElement(): FileRestore {
    if (this.restoreRealSource() === "nothing_to_restore") return "nothing_to_restore";
    if (this.#declaration === "markup" || this.#currentSrc === "") return "restored";
    this.#writeOwnSource(this.#originals.media.src, this.#currentSrc);
    return "restored_with_address";
  }

  /** Страница сама записала `srcObject` (оригиналом): наш поток заменён, возвращать нечего. */
  notePageReplacedSrcObject(): void {
    this.#ownsRealSource = false;
  }

  /** Разобрать доверенное `emptied` настоящего элемента. */
  classifyEmptied(): EmptiedOrigin {
    if (this.#ownEmptiedPending === 0) return "page_source_change";
    this.#ownEmptiedPending -= 1;
    return "own_write";
  }

  // ------------------------------------------------------------------ ShadowSourceView

  duration(): number {
    return this.#durationSeconds;
  }

  buffered(): TimeRangesShape {
    return this.#ranges();
  }

  seekable(): TimeRangesShape {
    return this.#ranges();
  }

  networkState(): number {
    return NETWORK_IDLE;
  }

  currentSrc(): string {
    return this.#currentSrc;
  }

  srcObject(): unknown {
    // Файл — это атрибут `src`/`<source>`: свой `srcObject` страница не ставила.
    return null;
  }

  videoWidth(): number {
    return this.#videoWidth;
  }

  videoHeight(): number {
    return this.#videoHeight;
  }

  /**
   * Своя запись источника: считает будущее доверенное `emptied` (оно приходит, только если у
   * элемента был источник) и снимает «можно автозапуск» — иначе элемент с атрибутом `autoplay`
   * сам заиграл бы после загрузки (оригинальный `pause()` по стандарту сбрасывает этот флаг).
   */
  #writeOwnSource(accessor: OriginalAccessor, value: object | string | null): void {
    const willFireEmptied = Number(readOriginal(this.#originals.media.networkState, this.#element)) !== NETWORK_EMPTY;
    writeOriginal(accessor, this.#element, value);
    if (willFireEmptied) this.#ownEmptiedPending += 1;
    callOriginal(this.#originals.media.pause, this.#element);
  }
}
