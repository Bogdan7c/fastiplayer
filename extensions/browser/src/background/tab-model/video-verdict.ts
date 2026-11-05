// Классификатор видео вкладки: чистая функция «факты кандидата + правила → вердикт».
// Единственное место, где решается «фильм это, реклама или мусор»: выбор главного видео
// (handoff/video-choice.ts) и модель вкладки используют ровно эту функцию — второго правила нет.

import type { VideoCandidate } from "../../shared/handoff-contract.ts";
import { VIDEO_FILTER_RULES, type VideoFilterRules } from "./video-filter-rules.ts";

/**
 * Вердикт о видео:
 * - `main` — кандидат в главное видео;
 * - `ad` — отдельный рекламный ролик: рекламный домен или преролл рядом с ожидающим фильмом;
 * - `junk` — баннер, превью по наведению, короткий ролик;
 * - `protected` — под DRM, не трогаем (инвариант 4);
 * - `unsupported` — форма источника, которую мы не умеем передавать (not supported yet);
 * - `empty` — источника нет (или страница только положила адрес и видео ещё не запускала).
 */
export type VideoVerdict = "main" | "ad" | "junk" | "protected" | "unsupported" | "empty";

/** Хост равен рекламному суффиксу или является его поддоменом (сравнение без учёта регистра). */
export function isAdHost(host: string | null, rules: VideoFilterRules): boolean {
  if (host === null || host === "") return false;
  const normalized = host.toLowerCase();
  return rules.adHostSuffixes.some((suffix) => normalized === suffix || normalized.endsWith(`.${suffix}`));
}

/** Беззвучное видео с признаком «не плеер»: цикл, мелкая плитка или короткий ролик. */
function isSilentDecoration(candidate: VideoCandidate, rules: VideoFilterRules): boolean {
  if (!candidate.muted) return false;
  const isShort = candidate.duration_s !== null && candidate.duration_s < rules.shortClipSeconds;
  return candidate.loop || candidate.visible_area_px < rules.smallAreaPx || isShort;
}

/**
 * Адрес плейлиста лежит в `src`, а видео ещё никто не запускал: ни длительности, ни игры. Это не
 * «формат не поддержан», а «ещё не началось»: Ashdi держит `.m3u8` в `src` только до нажатия play,
 * потом уходит на MediaSource. Настоящий нативный HLS играет или хотя бы знает длительность.
 */
function isUnstartedPlaylistAddress(candidate: VideoCandidate): boolean {
  const { media, playing, duration_s: durationSeconds } = candidate;
  return media.kind === "unsupported" && media.form === "native_hls" && !playing && durationSeconds === null;
}

/**
 * Сосед по документу ждёт фильм, пока `clip` играет поверх него:
 * - стоящий MediaSource длиннее ролика (Ashdi: фильм уже загружен и ждёт конца рекламы);
 *   длительность MediaSource бывает ещё неизвестна — это тоже ожидание;
 * - стоящий незапущенный `<video>` той же площади: пустой (Tortuga снял источник на время рекламы)
 *   или с адресом плейлиста (Tortuga на странице с двумя плеерами). Без равной площади это мог бы
 *   быть другой элемент ленты, поэтому площадь обязательна.
 */
function waitsForFilm(neighbour: VideoCandidate, clip: VideoCandidate, clipSeconds: number): boolean {
  if (neighbour.playing) return false;
  if (neighbour.media.kind === "media_source") return (neighbour.duration_s ?? Infinity) > clipSeconds;
  const unstarted = neighbour.media.kind === "none" || isUnstartedPlaylistAddress(neighbour);
  return unstarted && neighbour.visible_area_px > 0 && neighbour.visible_area_px === clip.visible_area_px;
}

/**
 * Преролл с неизвестного домена: играет незацикленный файл, короткий для фильма, а рядом ждёт фильм.
 * Список доменов такие ролики не ловит: сайты крутят рекламу через местные сети, домены которых
 * меняются. Устройство страницы от домена не зависит. Цикл исключён: так играют баннеры и плитки,
 * а не реклама, которую надо досмотреть. Ошибка правила стоит недорого: настоящий короткий ролик
 * получит признак рекламы, а когда решение 206 заблокирует перемотку, зритель не сможет его мотать.
 */
function isPrerollBesideFilm(candidate: VideoCandidate, neighbours: readonly VideoCandidate[], rules: VideoFilterRules): boolean {
  const { media, playing, loop, duration_s: clipSeconds } = candidate;
  if (media.kind !== "file" || !playing || loop) return false;
  if (clipSeconds === null || clipSeconds > rules.prerollMaxSeconds) return false;
  return neighbours.some((neighbour) => waitsForFilm(neighbour, candidate, clipSeconds));
}

/**
 * Классификация одного кандидата. Порядок проверок — часть правила, менять осознанно.
 * `neighbours` — другие видео того же документа: преролл виден только рядом с ожидающим фильмом.
 */
export function classifyVideo(
  candidate: VideoCandidate,
  rules: VideoFilterRules = VIDEO_FILTER_RULES,
  neighbours: readonly VideoCandidate[] = [],
): VideoVerdict {
  if (candidate.drm) return "protected";
  if (candidate.media.kind === "none" || isUnstartedPlaylistAddress(candidate)) return "empty";
  if (candidate.media.kind === "unsupported") return "unsupported";
  if (isAdHost(candidate.file_host, rules) || isAdHost(candidate.frame_host, rules)) return "ad";
  // До проверки на беззвучие: преролл на странице без звука всё равно реклама, а не баннер.
  if (isPrerollBesideFilm(candidate, neighbours, rules)) return "ad";
  if (isSilentDecoration(candidate, rules)) return "junk";
  // Известно, что ролик короткий, и он стоит: не фильм (играющий короткий со звуком оставляем).
  if (candidate.duration_s !== null && candidate.duration_s < rules.shortClipSeconds && !candidate.playing) return "junk";
  return "main";
}
