// Время сегмента фрагментированного MP4: с какой секунды потока начинается медиасегмент (решение 364).
//
// Зачем: потолок «памяти без копий» отдаёт сначала уже просмотренное. Чтобы знать, что запись журнала
// лежит ПОЗАДИ текущей секунды страницы, надо знать время сегмента, с которого начнётся повтор после
// выкидывания. Сканер границ (fmp4-scanner.ts) времени не читает: он перепрыгивает тела боксов.
//
// Как читаем. Время декодирования первого кадра фрагмента лежит в `moof → traf → tfdt`
// (`baseMediaDecodeTime`, в единицах шкалы дорожки). Шкала дорожки («единиц в секунде») лежит в
// init-сегменте: `moov → trak → mdia → mdhd` (`timescale`), номер дорожки — в `trak → tkhd`.
// Секунды = baseMediaDecodeTime / timescale.
//
// Когда читаем: лениво, только когда потолок решает, можно ли выкинуть запись (а не на каждый
// `appendBuffer`), и только заголовки: тела боксов не трогаются. Байты достаёт `StreamByteReader`
// владельца (журнал, дорожка): у них виды на память страницы, и только они знают, цел ли буфер.
//
// Любая странность (бокс не тот, размер не сходится, байтов ещё нет, буфер отсоединён) — честное
// «время неизвестно» (`null`). Неизвестное время никогда не считается «просмотренным».

/**
 * Достать `length` байт потока SourceBuffer со смещения `offset` (копия нескольких байт заголовка).
 * `null` — этих байт у владельца нет (ещё не пришли, уже выкинуты, буфер страницы отсоединён).
 */
export type StreamByteReader = (offset: number, length: number) => Uint8Array | null;

/** Что фрагмент говорит о своём начале. */
export interface FragmentDecodeTime {
  /** Номер дорожки (`track_ID` из `tfhd`): по нему выбирается шкала в init-сегменте. */
  readonly trackId: number;
  /** `baseMediaDecodeTime` в единицах шкалы дорожки. */
  readonly decodeTime: number;
}

/** Длина обычного заголовка бокса: size + type. */
const BOX_HEADER_BYTES = 8;
/** Длина заголовка с 64-битным `largesize`. */
const LARGE_BOX_HEADER_BYTES = 16;
/** `size == 1` — настоящий размер в `largesize`. */
const LARGESIZE_MARKER = 1;
/** Версия и флаги «полного» бокса (FullBox): 1 + 3 байта перед полями. */
const FULL_BOX_PREFIX_BYTES = 4;
/** Версия 1 полных боксов хранит времена 64-битными числами. */
const WIDE_VERSION = 1;
/**
 * Сколько боксов одного уровня просматриваем, прежде чем сдаться. Перед `moof` в сегменте бывают
 * `styp`, `sidx`, `prft` и несколько `emsg`; в `moov` — `mvhd`, дорожки, `mvex`, `udta`. 64 — с
 * большим запасом, а мусорный поток не заставит ходить по нему бесконечно.
 */
const MAX_SIBLING_BOXES = 64;

interface Box {
  readonly type: string;
  /** Смещение первого байта тела. */
  readonly bodyStart: number;
  /** Смещение сразу за боксом. */
  readonly end: number;
}

function readUnsigned(bytes: Uint8Array, start: number, count: number): number {
  let value = 0;
  for (let index = start; index < start + count; index += 1) value = value * 256 + (bytes[index] ?? 0);
  return value;
}

/** Заголовок бокса на смещении `offset`; `null` — байтов нет или это не заголовок. */
function readBox(read: StreamByteReader, offset: number): Box | null {
  const header = read(offset, BOX_HEADER_BYTES);
  if (header === null) return null;
  const type = String.fromCharCode(header[4] ?? 0, header[5] ?? 0, header[6] ?? 0, header[7] ?? 0);
  let size = readUnsigned(header, 0, 4);
  let headerBytes = BOX_HEADER_BYTES;
  if (size === LARGESIZE_MARKER) {
    const largeSize = read(offset + BOX_HEADER_BYTES, LARGE_BOX_HEADER_BYTES - BOX_HEADER_BYTES);
    if (largeSize === null) return null;
    size = readUnsigned(largeSize, 0, 8);
    headerBytes = LARGE_BOX_HEADER_BYTES;
  }
  // `size == 0` («до конца файла») и размер меньше заголовка в потоке MSE — мусор.
  if (!Number.isSafeInteger(size) || size < headerBytes) return null;
  return { type, bodyStart: offset + headerBytes, end: offset + size };
}

/**
 * Боксы подряд с `start` до `end` (`end` — `null`, если конец неизвестен). Обход кончается на первом
 * непонятном заголовке или боксе, вылезающем за `end`: дальше верить смещениям нельзя.
 */
function* boxesBetween(read: StreamByteReader, start: number, end: number | null): Generator<Box> {
  let offset = start;
  for (let seen = 0; seen < MAX_SIBLING_BOXES; seen += 1) {
    if (end !== null && offset + BOX_HEADER_BYTES > end) return;
    const box = readBox(read, offset);
    if (box === null || (end !== null && box.end > end)) return;
    yield box;
    offset = box.end;
  }
}

function childOf(read: StreamByteReader, parent: Box, type: string): Box | null {
  for (const child of boxesBetween(read, parent.bodyStart, parent.end)) {
    if (child.type === type) return child;
  }
  return null;
}

/** 32-битное поле полного бокса, лежащее после `fieldsBefore` байт его полей. */
function readFullBoxField(read: StreamByteReader, box: Box, fieldsBefore: number): number | null {
  const field = read(box.bodyStart + FULL_BOX_PREFIX_BYTES + fieldsBefore, 4);
  return field === null ? null : readUnsigned(field, 0, 4);
}

/** Версия полного бокса (первый байт тела). */
function versionOf(read: StreamByteReader, box: Box): number | null {
  const version = read(box.bodyStart, 1);
  return version === null ? null : (version[0] ?? 0);
}

/**
 * Время декодирования первого фрагмента сегмента, который начинается на смещении `segmentStart`
 * (граница журнала: начало группы `styp`/`sidx`/`emsg`/`prft`/`moof` или начало init-сегмента).
 * Берётся первая дорожка фрагмента: у SourceBuffer почти всегда одна дорожка, а у общего потока
 * дорожки фрагмента начинаются вместе.
 */
export function readFmp4FragmentDecodeTime(read: StreamByteReader, segmentStart: number): FragmentDecodeTime | null {
  for (const box of boxesBetween(read, segmentStart, null)) {
    // Данные раньше фрагмента: это не начало сегмента, время такого места неизвестно.
    if (box.type === "mdat") return null;
    if (box.type !== "moof") continue;
    const trackFragment = childOf(read, box, "traf");
    if (trackFragment === null) return null;
    return decodeTimeOfTrackFragment(read, trackFragment);
  }
  return null;
}

function decodeTimeOfTrackFragment(read: StreamByteReader, trackFragment: Box): FragmentDecodeTime | null {
  const header = childOf(read, trackFragment, "tfhd");
  const decodeTimeBox = childOf(read, trackFragment, "tfdt");
  if (header === null || decodeTimeBox === null) return null;
  // tfhd: версия/флаги, затем track_ID.
  const trackId = readFullBoxField(read, header, 0);
  const version = versionOf(read, decodeTimeBox);
  if (trackId === null || version === null) return null;
  // tfdt: версия/флаги, затем baseMediaDecodeTime (64 бита в версии 1, иначе 32).
  const wide = version === WIDE_VERSION;
  const value = read(decodeTimeBox.bodyStart + FULL_BOX_PREFIX_BYTES, wide ? 8 : 4);
  if (value === null) return null;
  return { trackId, decodeTime: readUnsigned(value, 0, value.length) };
}

/**
 * Шкалы дорожек init-сегмента, лежащего в потоке с `initStart` до `initEnd`: номер дорожки → единиц
 * в секунде. `null` — `moov` не найден или не разобран.
 */
export function readFmp4TrackTimescales(read: StreamByteReader, initStart: number, initEnd: number): ReadonlyMap<number, number> | null {
  for (const box of boxesBetween(read, initStart, initEnd)) {
    if (box.type !== "moov") continue;
    const timescales = new Map<number, number>();
    for (const track of boxesBetween(read, box.bodyStart, box.end)) {
      if (track.type !== "trak") continue;
      const described = timescaleOfTrack(read, track);
      if (described !== null) timescales.set(described.trackId, described.timescale);
    }
    return timescales.size > 0 ? timescales : null;
  }
  return null;
}

function timescaleOfTrack(read: StreamByteReader, track: Box): { readonly trackId: number; readonly timescale: number } | null {
  const trackHeader = childOf(read, track, "tkhd");
  const media = childOf(read, track, "mdia");
  const mediaHeader = media === null ? null : childOf(read, media, "mdhd");
  if (trackHeader === null || mediaHeader === null) return null;
  const trackHeaderVersion = versionOf(read, trackHeader);
  const mediaHeaderVersion = versionOf(read, mediaHeader);
  if (trackHeaderVersion === null || mediaHeaderVersion === null) return null;
  // tkhd и mdhd начинаются одинаково: creation_time и modification_time (по 8 байт в версии 1, иначе
  // по 4). Сразу за ними у tkhd — track_ID, у mdhd — timescale.
  const timesBytes = (version: number): number => (version === WIDE_VERSION ? 16 : 8);
  const trackId = readFullBoxField(read, trackHeader, timesBytes(trackHeaderVersion));
  const timescale = readFullBoxField(read, mediaHeader, timesBytes(mediaHeaderVersion));
  if (trackId === null || timescale === null || timescale === 0) return null;
  return { trackId, timescale };
}
