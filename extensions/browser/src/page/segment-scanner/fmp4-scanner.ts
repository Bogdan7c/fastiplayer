// Сканер границ фрагментированного MP4 (fMP4, ISO BMFF): только верхние боксы.
//
// Заголовок бокса: `size` (4 байта, big-endian) + `type` (4 ASCII). `size == 1` — дальше 8 байт
// `largesize`; `size == 0` («до конца файла») в MSE-потоке смысла не имеет — считаем сбоем.
//
// Init-сегмент: от `ftyp` (или `moov`, если `ftyp` нет) до конца `moov`.
// Граница медиасегмента: начало группы боксов `styp`/`sidx`/`emsg`/`prft`/`moof` — первого такого
// бокса после бокса другого вида (`mdat`, `moov`, …) или в начале потока. Внутри группы
// (`styp` → `sidx` → `moof`) граница одна — на первом боксе: так при повторе сегмент не теряет
// свой `styp`/`emsg`.

import { HeaderWalker, type HeaderStep } from "./header-walker.ts";
import type { SegmentMarker } from "./scanner.ts";

/** Длина обычного заголовка: size + type. */
const BOX_HEADER_BYTES = 8;
/** Длина заголовка с 64-битным `largesize`. */
const LARGE_BOX_HEADER_BYTES = 16;
/** `size == 1` — значит, настоящий размер в `largesize`. */
const LARGESIZE_MARKER = 1;
/** `size == 0` — бокс до конца файла. */
const TO_END_OF_FILE_SIZE = 0;

/** Боксы, с которых начинается медиасегмент (ISO/IEC 23009-1 и 14496-12). */
const SEGMENT_START_TYPES: ReadonlySet<string> = new Set(["styp", "sidx", "emsg", "prft", "moof"]);

/**
 * Каким боксом может начинаться MSE-поток MP4. Всё остальное в начале потока — признак, что это
 * не fMP4 (мусор, другой контейнер с тем же MIME), и журнал такого потока не ведётся.
 */
const ALLOWED_FIRST_TYPES: ReadonlySet<string> = new Set([
  "ftyp",
  "moov",
  "styp",
  "sidx",
  "emsg",
  "prft",
  "moof",
  "free",
  "skip",
  "uuid",
]);

/** Тип бокса — четыре печатных ASCII-символа (иначе это не заголовок, а мусор). */
function isPrintableAscii(byte: number): boolean {
  return byte >= 0x20 && byte <= 0x7e;
}

export class Fmp4Scanner extends HeaderWalker {
  /** Встречался ли уже хоть один бокс (для проверки первого бокса потока). */
  #sawAnyBox = false;
  /** Предыдущий верхний бокс начинал медиасегмент (мы внутри группы styp/sidx/moof). */
  #insideSegmentStartGroup = false;
  /** Init-сегмент начат (`ftyp`), но `moov` ещё не кончился. */
  #initOpen = false;

  constructor(streamOffset: number) {
    super(streamOffset);
  }

  protected onHeaderByte(headerStart: number, markers: SegmentMarker[]): HeaderStep {
    if (this.header.length < BOX_HEADER_BYTES) return { kind: "need_more" };
    const size32 = this.header.readUnsigned(0, 4);
    if (size32 === LARGESIZE_MARKER && this.header.length < LARGE_BOX_HEADER_BYTES) return { kind: "need_more" };

    const typeBytes = [4, 5, 6, 7].map((index) => this.header.byteAt(index));
    if (!typeBytes.every(isPrintableAscii)) return { kind: "failed", reason: "mp4 box type is not ASCII" };
    const boxType = String.fromCharCode(...typeBytes);

    let boxSize: number;
    let headerLength: number;
    if (size32 === LARGESIZE_MARKER) {
      boxSize = this.header.readUnsigned(8, 8);
      headerLength = LARGE_BOX_HEADER_BYTES;
    } else if (size32 === TO_END_OF_FILE_SIZE) {
      return { kind: "failed", reason: "mp4 box size 0 (to end of file) in an MSE stream" };
    } else {
      boxSize = size32;
      headerLength = BOX_HEADER_BYTES;
    }
    if (!Number.isSafeInteger(boxSize) || boxSize < headerLength) {
      return { kind: "failed", reason: "mp4 box size is invalid" };
    }
    if (!this.#sawAnyBox && !ALLOWED_FIRST_TYPES.has(boxType)) {
      return { kind: "failed", reason: "stream does not start like fragmented mp4" };
    }
    this.#sawAnyBox = true;

    const bodyBytes = boxSize - headerLength;
    const startsSegment = SEGMENT_START_TYPES.has(boxType);
    let markerAtBodyEnd: "init_end" | null = null;

    if (boxType === "ftyp" || boxType === "moov") {
      if (!this.#initOpen) {
        markers.push({ kind: "init_start", offset: headerStart });
        this.#initOpen = true;
      }
      if (boxType === "moov") {
        markerAtBodyEnd = "init_end";
        this.#initOpen = false;
      }
    } else if (startsSegment && !this.#insideSegmentStartGroup) {
      markers.push({ kind: "media_start", offset: headerStart });
    }
    this.#insideSegmentStartGroup = startsSegment;
    return { kind: "skip_body", bodyBytes, markerAtBodyEnd };
  }

  protected onRestart(): void {
    // После abort() браузер ждёт новый бокс: прерванный init забыт, группа сегмента — тоже.
    this.#insideSegmentStartGroup = false;
    this.#initOpen = false;
  }
}
