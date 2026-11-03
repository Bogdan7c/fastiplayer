// Разбор MIME-строки плеера страницы (`video/mp4; codecs="avc1.64001f, mp4a.40.2"`) в ответ на вопрос
// «какие видео-кодеки здесь заказаны». Чистые функции без состояния; страница — враг, поэтому на любой
// мусор ответ «не знаю» (`unknown`) — тогда решение остаётся за браузером.
import type { RefusableFamily, VideoCodecFamily } from "../../shared/codec-policy.ts";

/** Семейства видео, которые различает классификатор (`vp8` и `other_video` вне метки плеера). */
export type ClassifiedVideoFamily = VideoCodecFamily | "vp8" | "other_video";

/** Что строка заказывает. */
export type MimeVerdict =
  | { readonly kind: "no_video" } // только звук: политика его не трогает
  | { readonly kind: "unknown" } // не разобрали или кодеки не названы: решает браузер
  | { readonly kind: "video"; readonly families: readonly ClassifiedVideoFamily[] };

/** Типы плейлистов HLS (решение 64): страница должна уйти в MSE / hls.js, а не в нативный HLS. */
const HLS_MIME_TYPES: ReadonlySet<string> = new Set([
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/mpegurl",
  "audio/x-mpegurl",
]);

/** Верхняя граница длины разбираемой строки: плеер не присылает больше, а враг мог бы прислать мегабайт. */
const MAX_MIME_LENGTH = 2048;

/** Префикс кодека (до первой точки, в нижнем регистре) -> семейство видео. */
const VIDEO_CODEC_PREFIXES: ReadonlyMap<string, ClassifiedVideoFamily> = new Map([
  ["avc1", "h264"],
  ["avc2", "h264"],
  ["avc3", "h264"],
  ["avc4", "h264"],
  ["hev1", "hevc"],
  ["hvc1", "hevc"],
  ["hevc", "hevc"],
  ["dvh1", "hevc"], // Dolby Vision поверх HEVC
  ["dvhe", "hevc"],
  ["vp09", "vp9"],
  ["vp9", "vp9"],
  ["vp08", "vp8"],
  ["vp8", "vp8"],
  ["av01", "av1"],
  ["av1", "av1"],
  ["theora", "other_video"],
  ["mp4v", "other_video"],
  ["vc-1", "other_video"],
]);

/** Звуковые кодеки: узнаём их, чтобы строка только со звуком была «без видео», а не «непонятно». */
const AUDIO_CODEC_PREFIXES: ReadonlySet<string> = new Set([
  "mp4a", "opus", "vorbis", "flac", "ac-3", "ec-3", "mp3", "alac", "dtsc", "dtse", "dtsh", "dtsl", "ac-4", "speex",
]);

/** MIME без списка кодеков, по которому всё же можно сказать семейство. */
const BARE_TYPE_FAMILIES: ReadonlyMap<string, ClassifiedVideoFamily> = new Map([
  // Голый WebM в практике сайтов — VP9 (VP8 — редкость): считаем семейством VP9.
  ["video/webm", "vp9"],
  ["video/ogg", "other_video"],
]);

/** Это тип плейлиста HLS (с любыми параметрами и в любом регистре)? */
export function isHlsMimeType(mime: string): boolean {
  if (mime.length > MAX_MIME_LENGTH) return false;
  const essence = (mime.split(";", 1)[0] ?? "").trim().toLowerCase();
  return HLS_MIME_TYPES.has(essence);
}

/** Что заказывает строка (кроме HLS — его проверяет `isHlsMimeType`). */
export function classifyMimeVideo(mime: string): MimeVerdict {
  if (mime.length > MAX_MIME_LENGTH) return { kind: "unknown" };
  const [essencePart = "", ...parameters] = mime.split(";");
  const essence = essencePart.trim().toLowerCase();
  if (!/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(essence)) return { kind: "unknown" };

  const codecTokens = readCodecTokens(parameters);
  if (codecTokens === undefined) return classifyWithoutCodecs(essence);

  const videoFamilies = new Set<ClassifiedVideoFamily>();
  let recognizedAudio = 0;
  let unrecognized = 0;
  for (const token of codecTokens) {
    const prefix = (token.split(".", 1)[0] ?? "").toLowerCase();
    const videoFamily = VIDEO_CODEC_PREFIXES.get(prefix);
    if (videoFamily !== undefined) videoFamilies.add(videoFamily);
    else if (AUDIO_CODEC_PREFIXES.has(prefix)) recognizedAudio += 1;
    else unrecognized += 1;
  }
  if (videoFamilies.size > 0) return { kind: "video", families: [...videoFamilies] };
  // Видео не нашли: «без видео» только если всё остальное распознано как звук.
  return recognizedAudio > 0 && unrecognized === 0 ? { kind: "no_video" } : { kind: "unknown" };
}

/** Список кодеков из параметра `codecs=...`; `undefined` — параметра нет или он пуст. */
function readCodecTokens(parameters: readonly string[]): readonly string[] | undefined {
  for (const parameter of parameters) {
    const separator = parameter.indexOf("=");
    if (separator < 0) continue;
    if (parameter.slice(0, separator).trim().toLowerCase() !== "codecs") continue;
    const value = parameter.slice(separator + 1).trim().replace(/^(["'])(.*)\1$/s, "$2");
    const tokens = value.split(",").map((token) => token.trim()).filter((token) => token.length > 0);
    return tokens.length > 0 ? tokens : undefined;
  }
  return undefined;
}

/** Кодеки не названы: по типу можно судить лишь для WebM/Ogg и для `audio/*`. */
function classifyWithoutCodecs(essence: string): MimeVerdict {
  const bareFamily = BARE_TYPE_FAMILIES.get(essence);
  if (bareFamily !== undefined) return { kind: "video", families: [bareFamily] };
  // `video/mp4` без кодеков может быть и H.264 — это решает браузер.
  return essence.startsWith("audio/") ? { kind: "no_video" } : { kind: "unknown" };
}

/** Какие из заказанных в строке семейств политика отвергает (пусто — строку не трогаем). */
export function refusedFamilies(mime: string, refused: readonly RefusableFamily[]): readonly ClassifiedVideoFamily[] {
  const verdict = classifyMimeVideo(mime);
  if (verdict.kind !== "video") return [];
  return verdict.families.filter((family) => (refused as readonly string[]).includes(family));
}
