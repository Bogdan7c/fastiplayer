// Белый список руки-функции YouTube (MAIN-мир): пять действий над активным плеером и ничего больше.
//
// Все функции плеера читаем по имени и зовём с `this` = элемент плеера. Любое из действий может бросить
// (плеер YouTube чужой) — это ловит исполнитель (`src/page/adapter-hand.ts`) и отвечает `failed`.

import {
  PAGE_FUNCTION_MISSING,
  pageDocumentOf,
  type PlayerHandActions,
} from "../../capabilities/player-hand/contract.ts";
import {
  VARIANT_KEY_PATTERN,
  YOUTUBE_HAND_ACTIONS,
  YOUTUBE_PLAYER_FUNCTIONS,
  YOUTUBE_VARIANT_MARKUP,
} from "./variant-markup.ts";
import { describeCaptions, selectCaptions } from "./captions-hand.ts";
import { call, listFrom, nonEmptyString, read } from "./page-reading.ts";

const functions = YOUTUBE_PLAYER_FUNCTIONS;
const markup = YOUTUBE_VARIANT_MARKUP;

/**
 * Активный плеер: среди всех `.html5-video-player` первый, у которого `getVideoData().video_id` — непустая
 * строка. В Shorts пустой `#movie_player` стоит в документе раньше активного — по id искать нельзя.
 * `undefined` — документа нет или активного плеера нет (страница строится, ролик не открыт).
 */
function findActivePlayer(page: object): object | undefined {
  const documentOfPage = pageDocumentOf(page);
  if (documentOfPage === undefined) return undefined;
  for (const candidate of Array.from(documentOfPage.querySelectorAll(markup.playerSelector)) as object[]) {
    const videoData = call(candidate, functions.videoData);
    if (nonEmptyString(read(videoData, "video_id"))) return candidate;
  }
  return undefined;
}

/** Шортс? Активный плеер Shorts или путь страницы `/shorts…`. */
function isShortsPage(page: object, player: object): boolean {
  if (read(player, "id") === markup.shortsPlayerId) return true;
  const path = read(read(page, "location"), "pathname");
  return typeof path === "string" && path.startsWith(markup.shortsPathPrefix);
}

/**
 * Описание дорожки озвучки. Сайт кладёт его во ВЛОЖЕННЫЙ объект под СЖАТЫМ именем поля (видели `Tb`, имя
 * меняется между сборками), поэтому ищем по форме: собственное поле-объект с `name: string` и `id: string`.
 */
function audioDescriptor(track: unknown): { readonly name: string; readonly id: string } | undefined {
  if (typeof track !== "object" || track === null) return undefined;
  for (const key of Object.keys(track)) {
    const nested = Reflect.get(track, key);
    const name = read(nested, "name");
    const id = read(nested, "id");
    if (typeof name === "string" && typeof id === "string") return { name, id };
  }
  return undefined;
}

/** Код языка дорожки: часть `id` до первой точки (`"zh-Hant.3"` → `"zh-Hant"`). */
function languageOf(track: unknown): string | undefined {
  const descriptor = audioDescriptor(track);
  if (descriptor === undefined) return undefined;
  const language = descriptor.id.split(".")[0] ?? "";
  return language === "" ? undefined : language;
}

/** Проигрываемые качества плеера (`isPlayable: false` пропускаем) в виде `{level, label}`. */
function playableQualities(player: object): readonly { readonly level: string; readonly label: string }[] {
  return listFrom(call(player, functions.availableQualities)).flatMap((entry) => {
    const level = read(entry, "quality");
    const label = read(entry, "qualityLabel");
    if (typeof level !== "string" || typeof label !== "string" || read(entry, "isPlayable") === false) return [];
    return [{ level, label }];
  });
}

/** Описание активного плеера простыми данными (то, что `HandValue` пропускает). */
function describePlayer(page: object): unknown {
  const player = findActivePlayer(page);
  if (player === undefined) return null;
  const videoData = call(player, functions.videoData);
  const currentLanguage = languageOf(call(player, functions.currentAudioTrack));
  const preference = call(player, functions.qualityPreference);
  const audio = listFrom(call(player, functions.availableAudioTracks)).flatMap((track) => {
    const descriptor = audioDescriptor(track);
    const language = languageOf(track);
    return descriptor === undefined || language === undefined ? [] : [{ language, label: descriptor.name, selected: language === currentLanguage }];
  });
  return {
    videoId: read(videoData, "video_id"),
    live: read(videoData, "isLive") === true,
    shorts: isShortsPage(page, player),
    qualities: playableQualities(player),
    qualityPreference: typeof preference === "string" ? preference : null,
    audio,
    captions: describeCaptions(player),
  };
}

/** Выбор качества: `auto` возвращает автоматику; уровня нет среди проигрываемых — `false`. */
function selectQuality(page: object, level: string | null): unknown {
  if (level === null || !VARIANT_KEY_PATTERN.test(level)) return false;
  const player = findActivePlayer(page);
  if (player === undefined) return PAGE_FUNCTION_MISSING;
  if (level !== markup.autoQualityLevel && !playableQualities(player).some((quality) => quality.level === level)) return false;
  const outcome = call(player, functions.setQualityRange, level, level);
  return outcome === PAGE_FUNCTION_MISSING ? outcome : true;
}

/** Выбор озвучки по коду языка: ищем дорожку в списке плеера и отдаём ему ЕЁ ЖЕ (не копию). */
function selectAudio(page: object, language: string | null): unknown {
  if (language === null || !VARIANT_KEY_PATTERN.test(language)) return false;
  const player = findActivePlayer(page);
  if (player === undefined) return PAGE_FUNCTION_MISSING;
  const tracks = call(player, functions.availableAudioTracks);
  if (tracks === PAGE_FUNCTION_MISSING) return tracks;
  const track = listFrom(tracks).find((candidate) => languageOf(candidate) === language);
  if (track === undefined) return false;
  const outcome = call(player, functions.setAudioTrack, track);
  if (outcome === PAGE_FUNCTION_MISSING) return outcome;
  return outcome !== false;
}

/** Следующий ролик. В Shorts — `false`: ленты — отдельная сессия (16b). */
function nextVideo(page: object): unknown {
  const player = findActivePlayer(page);
  if (player === undefined) return PAGE_FUNCTION_MISSING;
  if (isShortsPage(page, player)) return false;
  const outcome = call(player, functions.nextVideo);
  return outcome === PAGE_FUNCTION_MISSING ? outcome : true;
}

export const YOUTUBE_PLAYER_HAND: PlayerHandActions = {
  [YOUTUBE_HAND_ACTIONS.describePlayer]: (page) => describePlayer(page),
  [YOUTUBE_HAND_ACTIONS.selectQuality]: (page, argument) => selectQuality(page, argument),
  [YOUTUBE_HAND_ACTIONS.selectAudio]: (page, argument) => selectAudio(page, argument),
  [YOUTUBE_HAND_ACTIONS.nextVideo]: (page) => nextVideo(page),
  [YOUTUBE_HAND_ACTIONS.selectCaptions]: (page, argument) => {
    const player = findActivePlayer(page);
    return player === undefined ? PAGE_FUNCTION_MISSING : selectCaptions(player, argument);
  },
};
