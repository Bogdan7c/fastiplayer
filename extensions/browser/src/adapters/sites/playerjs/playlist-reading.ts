// Чтение плейлиста PlayerJS из разметки (сессия 15b): чистые функции над взглядом на DOM, без нажатий.
//
// Три шага, каждый отдельной функцией:
// 1. `readPlaylist` — все пункты плейлиста по уровням дерева и список дорожек звука, как их рисует страница;
// 2. `playlistRolesOf` — что значит каждый уровень на этом сайте: озвучки, сезоны, серии (профиль хоста,
//    решение 296: на незнакомом сайте папки озвучками не считаем, серии — только по подписям);
// 3. `playingEntriesOf` — какой пункт каждого уровня ИГРАЕТ. Если меню страницы «на полпути» (папку
//    открыли, а серию ещё не выбрали — так устроена смена озвучки Ashdi), ответ — `unsettled`: открытая
//    папка ещё не то, что на экране, и в наше меню и в память выбора она попасть не должна.

import { normalizeText } from "../../capabilities/ad-organs/ad-text.ts";
import type { AdElement } from "../../capabilities/ad-organs/contract.ts";
import type { VariantPageView } from "../../capabilities/variants/contract.ts";
import { PLAYERJS_EPISODE_LABEL_PATTERN, PLAYERJS_PLAYLIST_MARKUP as markup, type PlayerJsHostProfile } from "./variant-markup.ts";

/** Пункт плейлиста или дорожки звука, как его нарисовала страница. */
export interface PlaylistEntry {
  readonly element: AdElement;
  /** Путь пункта (`me`): `x-1`, `xxx-0-1-6-300483`, `audio_1`. Годится в id протокола как есть. */
  readonly path: string;
  /** Подпись страницы без лишних пробелов (у Ashdi бывает ведущий пробел). */
  readonly label: string;
  /** Пункт помечен страницей как выбранный. */
  readonly active: boolean;
  /** Числа пути: у `xx-1-0` это `[1, 0]`; у дорожки звука — `[номер]`. Начало пути ребёнка = путь папки. */
  readonly numbers: readonly number[];
}

/** Плейлист страницы: уровни дерева (глубина → пункты) и отдельный список дорожек звука. */
export interface PlaylistReading {
  readonly levels: ReadonlyMap<number, readonly PlaylistEntry[]>;
  readonly audioTracks: readonly PlaylistEntry[];
  /** Хоть один заголовок списка показывает «...»: в открытой папке ничего не выбрано. */
  readonly pendingHead: boolean;
}

/** Что значат уровни дерева на этом сайте. Уровень без роли адаптер не трогает. */
export interface PlaylistRoles {
  readonly dubDepth?: number;
  readonly seasonDepth?: number;
  readonly episodeDepth?: number;
}

/** Какие пункты играют сейчас. */
export interface PlayingEntries {
  readonly dub?: PlaylistEntry;
  readonly season?: PlaylistEntry;
  readonly episode?: PlaylistEntry;
}

export type PlayingState = { readonly kind: "settled"; readonly entries: PlayingEntries } | { readonly kind: "unsettled" };

/** Выбран ли пункт: класс `pjs-<плеер>-active-pl` (имя плеера у каждого сайта своё, поэтому — по суффиксу). */
function isActive(view: VariantPageView, element: AdElement): boolean {
  const classes = view.attributeOf(element, markup.classAttribute) ?? "";
  return classes.split(/\s+/u).some((name) => name.startsWith("pjs-") && name.endsWith(markup.activeClassSuffix));
}

/** Пункт плейлиста по элементу и уже разобранному пути. */
function entryOf(view: VariantPageView, element: AdElement, path: string, numbers: readonly number[]): PlaylistEntry {
  return { element, path, label: normalizeText(view.textOf(element)), active: isActive(view, element), numbers };
}

/** Числа пути через дефис; путь уже проверен шаблоном, поэтому каждое — небольшое целое. */
function numbersOf(text: string): number[] {
  return text.split("-").map(Number);
}

/** Шаг 1: все пункты плейлиста страницы. Заголовки списков в пункты не входят, но их «...» запоминаем. */
export function readPlaylist(view: VariantPageView): PlaylistReading {
  const levels = new Map<number, PlaylistEntry[]>();
  const audioTracks: PlaylistEntry[] = [];
  let pendingHead = false;
  for (const element of view.findAll(markup.itemSelector)) {
    const path = view.attributeOf(element, markup.pathAttribute) ?? "";
    if (path.startsWith(markup.headPrefix)) {
      if (normalizeText(view.textOf(element)) === markup.pendingHeadLabel) pendingHead = true;
      continue;
    }
    const audioMatch = markup.audioTrackPathPattern.exec(path);
    if (audioMatch !== null) {
      audioTracks.push(entryOf(view, element, path, [Number(audioMatch[1])]));
      continue;
    }
    const playlistMatch = markup.playlistPathPattern.exec(path);
    if (playlistMatch === null) continue; // незнакомый `me` — не наш пункт
    const [, depthMarks = "", numberText = ""] = playlistMatch;
    const depth = depthMarks.length;
    const level = levels.get(depth) ?? [];
    level.push(entryOf(view, element, path, numbersOf(numberText)));
    levels.set(depth, level);
  }
  return { levels, audioTracks, pendingHead };
}

/** Все ли подписи уровня похожи на серии (для незнакомого сайта, решение 296). */
function looksLikeEpisodes(entries: readonly PlaylistEntry[]): boolean {
  return entries.length > 0 && entries.every((entry) => PLAYERJS_EPISODE_LABEL_PATTERN.test(entry.label));
}

/**
 * Шаг 2: роли уровней. `frameSearch` — строка запроса адреса фрейма: признак папок-озвучек Ashdi
 * (`?multivoice`). Уровней-не-озвучек больше двух — устройство незнакомо, серий не называем.
 */
export function playlistRolesOf(reading: PlaylistReading, profile: PlayerJsHostProfile, frameSearch: string): PlaylistRoles {
  const depths = [...reading.levels.keys()].sort((left, right) => left - right);
  const dubLevel = profile.dubFolderLevel;
  const flagPresent = dubLevel !== undefined && new URLSearchParams(frameSearch).has(dubLevel.addressFlag);
  const dubDepth = flagPresent && depths.includes(dubLevel.depth) ? dubLevel.depth : undefined;
  const rest = depths.filter((depth) => depth !== dubDepth);
  const dubRole = dubDepth === undefined ? {} : { dubDepth };
  const episodeDepth = rest.at(-1);
  if (episodeDepth === undefined || rest.length > 2) return dubRole;
  const episodes = reading.levels.get(episodeDepth) ?? [];
  if (!profile.episodesTrusted && !looksLikeEpisodes(episodes)) return dubRole;
  const seasonDepth = rest.length === 2 ? rest[0] : undefined;
  return seasonDepth === undefined ? { ...dubRole, episodeDepth } : { ...dubRole, seasonDepth, episodeDepth };
}

/** Единственный выбранный пункт уровня; нет уровня — `null` (роль не нужна), не один выбранный — `undefined`. */
function activeEntryAt(reading: PlaylistReading, depth: number | undefined): PlaylistEntry | null | undefined {
  if (depth === undefined) return null;
  const active = (reading.levels.get(depth) ?? []).filter((entry) => entry.active);
  return active.length === 1 ? active[0] : undefined;
}

/**
 * Шаг 3: что играет. `playlistId` — ответ `api("playlist_id")`. У сайта, где лист несёт id файла (Ashdi),
 * выбранный лист должен совпасть с играющим файлом: иначе страница показывает открытую папку, а играет
 * прежний файл.
 */
export function playingEntriesOf(reading: PlaylistReading, roles: PlaylistRoles, profile: PlayerJsHostProfile, playlistId: string | null): PlayingState {
  if (reading.pendingHead) return { kind: "unsettled" };
  const dub = activeEntryAt(reading, roles.dubDepth);
  const season = activeEntryAt(reading, roles.seasonDepth);
  const episode = activeEntryAt(reading, roles.episodeDepth);
  if (dub === undefined || season === undefined || episode === undefined) return { kind: "unsettled" };
  const leaf = episode ?? dub;
  if (profile.leafCarriesFileId && leaf !== null && String(leaf.numbers.at(-1)) !== playlistId) return { kind: "unsettled" };
  return {
    kind: "settled",
    entries: { ...(dub === null ? {} : { dub }), ...(season === null ? {} : { season }), ...(episode === null ? {} : { episode }) },
  };
}

/** Пункты уровня `depth`, лежащие в папке `parent` (путь ребёнка начинается с чисел папки). */
export function childrenOf(reading: PlaylistReading, depth: number, parent: PlaylistEntry): readonly PlaylistEntry[] {
  return (reading.levels.get(depth) ?? []).filter((entry) => parent.numbers.every((number, index) => entry.numbers[index] === number));
}

/**
 * Есть ли следующая серия в ТОЙ ЖЕ озвучке: следующая серия в списке или следующий сезон. Списки страницы
 * показывают только открытую папку, поэтому за концом последнего сезона озвучки Next не предлагаем: там
 * `api("next")` ушёл бы в другую озвучку.
 */
export function hasNextEpisode(reading: PlaylistReading, roles: PlaylistRoles, playing: PlayingEntries): boolean {
  if (roles.episodeDepth === undefined || playing.episode === undefined) return false;
  const isNotLast = (depth: number, entry: PlaylistEntry): boolean => {
    const level = reading.levels.get(depth) ?? [];
    return level.indexOf(entry) < level.length - 1;
  };
  if (isNotLast(roles.episodeDepth, playing.episode)) return true;
  return roles.seasonDepth !== undefined && playing.season !== undefined && isNotLast(roles.seasonDepth, playing.season);
}
