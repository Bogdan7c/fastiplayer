// Медиа для тестов сканера и журнала: настоящие файлы общих фикстур протокола и WebM, собранный
// вручную (свой мини-писатель EBML — независимо от сканера).
//
// Ожидаемые смещения верхних боксов/элементов файлов фикстур сверены независимым разбором
// (Python-скрипт по спецификациям ISO BMFF и EBML, без кода экстеншена) и ffprobe.

import { readFixtureBytes } from "../../protocol/fixtures-support.ts";

/** fMP4 H.264: ftyp(0) moov(28, до 733) moof(733) mdat(857) moof(5714) mdat(5838), всего 10093. */
export const VIDEO_MP4 = {
  bytes: (): Uint8Array => readFixtureBytes("media/video.mp4"),
  initEnd: 733,
  mediaStarts: [733, 5714],
} as const;

/** fMP4 AAC: ftyp(0) moov(28, до 692) moof(692) mdat(832) moof(1680) mdat(1860), всего 2722. */
export const AUDIO_MP4 = {
  bytes: (): Uint8Array => readFixtureBytes("media/audio.mp4"),
  initEnd: 692,
  mediaStarts: [692, 1680],
} as const;

/** fMP4 6 с, 5 фрагментов. */
export const VIDEO_64_6S_MP4 = {
  bytes: (): Uint8Array => readFixtureBytes("media/video-64-6s.mp4"),
  initEnd: 734,
  mediaStarts: [734, 7258, 13398, 18437, 24583],
} as const;

/**
 * WebM VP9+Opus (ffmpeg): EBML(0) Segment(36, размер известен, 8 байт) SeekHead(48) Void(111)
 * Info(209) Tracks(246) Tags(452) Cluster(623) Cues(7764).
 */
export const MUXED_WEBM = {
  bytes: (): Uint8Array => readFixtureBytes("media/muxed.webm"),
  initEnd: 623,
  mediaStarts: [623],
} as const;

// ------------------------------------------------------------------------------------------------
// Мини-писатель EBML: собирает WebM из элементов с известным или неизвестным размером.
// ------------------------------------------------------------------------------------------------

/** Размер «неизвестен»: 8-байтовая запись со всеми единицами в значении. */
const UNKNOWN_SIZE = new Uint8Array([0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);

function idBytes(elementId: number): Uint8Array {
  const bytes: number[] = [];
  let rest = elementId;
  while (rest > 0) {
    bytes.unshift(rest & 0xff);
    rest = Math.floor(rest / 256);
  }
  return new Uint8Array(bytes);
}

/** Размер записью из 4 байт (маркер 0x10): хватает для тестовых элементов. */
function sizeBytes(size: number): Uint8Array {
  return new Uint8Array([0x10 | ((size >>> 24) & 0x0f), (size >>> 16) & 0xff, (size >>> 8) & 0xff, size & 0xff]);
}

export function concat(parts: readonly Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const joined = new Uint8Array(total);
  let position = 0;
  for (const part of parts) {
    joined.set(part, position);
    position += part.length;
  }
  return joined;
}

/** Элемент с известным размером. */
export function ebmlElement(elementId: number, body: Uint8Array): Uint8Array {
  return concat([idBytes(elementId), sizeBytes(body.length), body]);
}

/** Заголовок элемента неизвестного размера (тело идёт следом отдельными элементами). */
export function ebmlUnknownSizeHeader(elementId: number): Uint8Array {
  return concat([idBytes(elementId), UNKNOWN_SIZE]);
}

export const EBML_IDS = {
  header: 0x1a45dfa3,
  segment: 0x18538067,
  info: 0x1549a966,
  tracks: 0x1654ae6b,
  cluster: 0x1f43b675,
  timestamp: 0xe7,
  simpleBlock: 0xa3,
  cues: 0x1c53bb6b,
} as const;

/** Заполнитель тела заданной длины (байты не важны сканеру — он их перепрыгивает). */
export function filler(length: number, seed = 7): Uint8Array {
  const bytes = new Uint8Array(length);
  for (let index = 0; index < length; index += 1) bytes[index] = (index * 31 + seed) & 0xff;
  return bytes;
}

/**
 * Живой WebM: Segment и кластеры неизвестного размера (как у живых трансляций), Cues в конце.
 * Возвращает байты и ожидаемые смещения: конец init и начала кластеров.
 */
export function liveWebm(clusterCount: number): { bytes: Uint8Array; initEnd: number; mediaStarts: number[] } {
  const parts: Uint8Array[] = [
    ebmlElement(EBML_IDS.header, filler(19)),
    ebmlUnknownSizeHeader(EBML_IDS.segment),
    ebmlElement(EBML_IDS.info, filler(25)),
    ebmlElement(EBML_IDS.tracks, filler(140)),
  ];
  const mediaStarts: number[] = [];
  let offset = parts.reduce((sum, part) => sum + part.length, 0);
  const initEnd = offset;
  for (let cluster = 0; cluster < clusterCount; cluster += 1) {
    const clusterParts = [
      ebmlUnknownSizeHeader(EBML_IDS.cluster),
      ebmlElement(EBML_IDS.timestamp, new Uint8Array([cluster])),
      ebmlElement(EBML_IDS.simpleBlock, filler(300 + cluster * 17, cluster)),
      ebmlElement(EBML_IDS.simpleBlock, filler(90, cluster + 3)),
    ];
    mediaStarts.push(offset);
    for (const part of clusterParts) {
      parts.push(part);
      offset += part.length;
    }
  }
  parts.push(ebmlElement(EBML_IDS.cues, filler(12)));
  return { bytes: concat(parts), initEnd, mediaStarts };
}

// ------------------------------------------------------------------------------------------------
// Нарезка потока на куски.
// ------------------------------------------------------------------------------------------------

/** Нарезать по заданным точкам (смещения внутри потока). Куски — виды без копий. */
export function splitAt(bytes: Uint8Array, cuts: readonly number[]): Uint8Array[] {
  const points = [0, ...cuts.filter((cut) => cut > 0 && cut < bytes.length), bytes.length];
  const chunks: Uint8Array[] = [];
  for (let index = 0; index + 1 < points.length; index += 1) {
    chunks.push(bytes.subarray(points[index], points[index + 1]));
  }
  return chunks;
}

/** Детерминированный генератор (одинаковые разрезы на каждом прогоне). */
export function seededRandom(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 0x1_0000_0000;
  };
}

/** Случайная нарезка на куски длиной 1..maxChunk. */
export function randomChunks(bytes: Uint8Array, maxChunk: number, seed: number): Uint8Array[] {
  const random = seededRandom(seed);
  const cuts: number[] = [];
  let position = 0;
  while (position < bytes.length) {
    position += 1 + Math.floor(random() * maxChunk);
    cuts.push(position);
  }
  return splitAt(bytes, cuts);
}
