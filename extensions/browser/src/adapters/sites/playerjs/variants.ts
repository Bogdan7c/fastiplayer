// Адаптер вариантов PlayerJS (изолированный мир, сессия 15b): озвучки, качество и Next на любом сайте с этим
// плеером (решение 290), осторожно на незнакомых (решение 296). Проверен вживую на Ashdi и Tortuga.
//
// Две руки:
// - рука-функция (`player-hand.ts`) — `api()` плеера: качества, озвучки-дорожки Tortuga, следующий файл;
// - рука DOM — чтение плейлиста (`playlist-reading.ts`) и «нажатие как человек» для озвучек-папок Ashdi
//   (`folder-dub-switch.ts`).
//
// Что уходит наружу:
// - seriesKey — хост и путь фрейма плеера (`ashdi.vip/serial/6320`): один сериал = одна запись памяти;
// - episodeKey — подписи играющих сезона и серии («Сезон 2/Серія 7»), а НЕ `playlist_id`: у Ashdi это id
//   файла, у каждой озвучки свой, и смена озвучки выглядела бы новой серией. У фильма — `null`;
// - озвучки и Next — только по тому, что ИГРАЕТ. Пока меню страницы «на полпути» (открыта чужая папка, в том
//   числе посреди нашей смены в три нажатия), адаптер отвечает последним устоявшимся описанием.

import { hostMatches } from "../../capabilities/ad-organs/ad-text.ts";
import type { AdFramePlace } from "../../capabilities/ad-organs/contract.ts";
import type {
  PageVariantDescription,
  PageVariantKind,
  PageVariantOption,
  PlayerHandResult,
  VariantActionOutcome,
  VariantAdapter,
  VariantHands,
} from "../../capabilities/variants/contract.ts";
import { optionFingerprintOf } from "../../capabilities/variants/option-fingerprint.ts";
import type { HandValue } from "../../../shared/adapter-hand-contract.ts";
import { switchFolderDub, type PlayerJsPause } from "./folder-dub-switch.ts";
import {
  hasNextEpisode,
  playingEntriesOf,
  playlistRolesOf,
  readPlaylist,
  type PlayingEntries,
  type PlaylistEntry,
  type PlaylistReading,
  type PlaylistRoles,
} from "./playlist-reading.ts";
import {
  PLAYERJS_AUTOMATIC_QUALITY_LABELS,
  PLAYERJS_HAND_ACTIONS,
  PLAYERJS_HOST_PROFILES,
  PLAYERJS_PLAYLIST_MARKUP,
  PLAYERJS_ROOT_SELECTOR,
  PLAYERJS_UNKNOWN_HOST_PROFILE,
  type PlayerJsHostProfile,
} from "./variant-markup.ts";

/** Рука-функция ответила не так, как ждёт адаптер: описание срывается целиком (полузнание хуже незнания). */
export class PlayerJsPageError extends Error {}

/** Что рука-функция узнала у `api()` плеера. */
interface PlayerFacts {
  readonly playlistId: string | null;
  readonly qualities: readonly string[] | null;
  readonly quality: string | null;
  readonly pathname: string;
  readonly search: string;
}

/** Плейлист и его смысл в один момент: из этого строится и описание, и любое действие. */
interface PageState {
  readonly facts: PlayerFacts;
  readonly reading: PlaylistReading;
  readonly roles: PlaylistRoles;
  /** `undefined` — меню страницы «на полпути»: что играет, по нему не понять. */
  readonly playing: PlayingEntries | undefined;
}

/** Настоящие часы для пауз между нажатиями. */
const REAL_PAUSE: PlayerJsPause = { sleep: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)) };

/** Префикс id пункта качества: `q<номер>_<отпечаток подписи>`. */
const QUALITY_ID_PREFIX = "q";

function isRecord(candidate: HandValue | undefined): candidate is Readonly<Record<string, HandValue>> {
  return typeof candidate === "object" && candidate !== null && !Array.isArray(candidate);
}

function stringOrNull(candidate: HandValue | undefined, field: string): string | null {
  if (candidate === null || typeof candidate === "string") return candidate;
  throw new PlayerJsPageError(`${field} is not a string`);
}

/** Ответ описания руки → факты; `null` — плеер ещё не готов. */
function playerFactsOf(value: HandValue): PlayerFacts | null {
  if (!isRecord(value)) throw new PlayerJsPageError("description is not an object");
  const { ready, playlistId, qualities, quality, pathname, search } = value;
  if (typeof ready !== "boolean" || typeof pathname !== "string" || typeof search !== "string") throw new PlayerJsPageError("description fields");
  if (!ready) return null;
  const qualityList = qualities === null ? null : Array.isArray(qualities) && qualities.every((item) => typeof item === "string") ? (qualities as string[]) : undefined;
  if (qualityList === undefined) throw new PlayerJsPageError("qualities is not a list of strings");
  return { playlistId: stringOrNull(playlistId, "playlistId"), qualities: qualityList, quality: stringOrNull(quality, "quality"), pathname, search };
}

/** Профиль хоста фрейма; незнакомый сайт — осторожный профиль (решение 296). */
function profileFor(hostname: string): PlayerJsHostProfile {
  return PLAYERJS_HOST_PROFILES.find((profile) => hostMatches(hostname, profile.hostSuffixes)) ?? PLAYERJS_UNKNOWN_HOST_PROFILE;
}

/** id пункта меню: путь или номер плюс отпечаток подписи — устаревший id не нажмёт соседа. */
function optionIdOf(key: string, label: string): string {
  return `${key}_${optionFingerprintOf(label)}`;
}

/** Разбор id пункта на ключ и отпечаток; без отпечатка — `undefined`. */
function parseOptionId(id: string): { readonly key: string; readonly fingerprint: string } | undefined {
  const separator = id.lastIndexOf("_");
  if (separator <= 0 || separator === id.length - 1) return undefined;
  return { key: id.slice(0, separator), fingerprint: id.slice(separator + 1) };
}

function dubOption(entry: PlaylistEntry, playing: PlaylistEntry | undefined): PageVariantOption {
  return { id: optionIdOf(entry.path, entry.label), label: entry.label, selected: entry === playing };
}

/** Озвучки: дорожки звука PlayerJS (Tortuga) или папки-озвучки проверенного сайта (Ashdi). */
function dubOptionsOf(state: PageState, playing: PlayingEntries): readonly PageVariantOption[] {
  const { reading, roles } = state;
  if (reading.audioTracks.length > 0) {
    const activeTrack = reading.audioTracks.find((track) => track.active);
    return reading.audioTracks.map((track) => dubOption(track, activeTrack));
  }
  if (roles.dubDepth === undefined) return [];
  return (reading.levels.get(roles.dubDepth) ?? []).map((entry) => dubOption(entry, playing.dub));
}

/** Качества из `api("qualities")`; меньше двух настоящих качеств — меню нет (одиночное ничего не даёт). */
function qualityOptionsOf(facts: PlayerFacts): readonly PageVariantOption[] {
  const qualities = facts.qualities ?? [];
  if (qualities.length < 2) return [];
  return qualities.map((label, index) => {
    const option: PageVariantOption = { id: optionIdOf(`${QUALITY_ID_PREFIX}${index}`, label), label, selected: label === facts.quality };
    const automatic = PLAYERJS_AUTOMATIC_QUALITY_LABELS.includes(label.trim().toLocaleLowerCase());
    return automatic ? { ...option, automatic } : option;
  });
}

/** Ключ серии по подписям играющих пунктов; серий нет (фильм) — `null`. */
function episodeKeyOf(playing: PlayingEntries): string | null {
  if (playing.episode === undefined) return null;
  return playing.season === undefined ? playing.episode.label : `${playing.season.label}/${playing.episode.label}`;
}

/** Исход действия руки, отвечающего `true` («сделано») или `false` («такого нет»). */
function handOutcome(result: PlayerHandResult): VariantActionOutcome {
  if (result.kind !== "done") return "failed";
  if (result.value === true) return "done";
  return result.value === false ? "no_such_variant" : "failed";
}

export function createPlayerJsVariants(hands: VariantHands, pause: PlayerJsPause = REAL_PAUSE): VariantAdapter {
  const { dom, player } = hands;
  /** Последнее описание, где меню страницы совпадало с тем, что играет. */
  let lastSettled: PageVariantDescription | null = null;

  /** Состояние страницы сейчас; `null` — плеера нет или он не готов. */
  async function readState(): Promise<PageState | null> {
    const result = await player.call(PLAYERJS_HAND_ACTIONS.describe, null);
    // Рука сказала «плеера нет» — страница не та или ещё строится.
    if (result.kind === "no_such_action") return null;
    if (result.kind !== "done") throw new PlayerJsPageError(`${PLAYERJS_HAND_ACTIONS.describe}: ${result.kind}`);
    const facts = playerFactsOf(result.value);
    if (facts === null) return null;
    const reading = readPlaylist(dom);
    const profile = profileFor(dom.hostname);
    const roles = playlistRolesOf(reading, profile, facts.search);
    const playingState = playingEntriesOf(reading, roles, profile, facts.playlistId);
    return { facts, reading, roles, playing: playingState.kind === "settled" ? playingState.entries : undefined };
  }

  function describeState(state: PageState, playing: PlayingEntries): PageVariantDescription {
    return {
      seriesKey: `${dom.hostname}${state.facts.pathname}`,
      episodeKey: episodeKeyOf(playing),
      audio: dubOptionsOf(state, playing),
      quality: qualityOptionsOf(state.facts),
      subtitles: [], // меню субтитров PlayerJS не ведём: документ приходит дорожкой (13b)
      nextEpisode: hasNextEpisode(state.reading, state.roles, playing),
    };
  }

  /** Озвучка-дорожка звука (Tortuga): `api("audiotrack", номер)`, если на этом месте та же подпись. */
  async function selectAudioTrack(state: PageState, key: string, fingerprint: string): Promise<VariantActionOutcome> {
    const track = state.reading.audioTracks.find((candidate) => candidate.path === key && optionFingerprintOf(candidate.label) === fingerprint);
    if (track === undefined) return "no_such_variant";
    return handOutcome(await player.call(PLAYERJS_HAND_ACTIONS.selectAudioTrack, String(track.numbers[0])));
  }

  /** Озвучка-папка (Ashdi): у фильма — одно нажатие, у сериала — три (`folder-dub-switch.ts`). */
  async function selectFolderDub(state: PageState, key: string, fingerprint: string): Promise<VariantActionOutcome> {
    const { roles, playing } = state;
    if (roles.dubDepth === undefined || playing === undefined) return "no_such_variant";
    const target = (state.reading.levels.get(roles.dubDepth) ?? []).find((entry) => entry.path === key && optionFingerprintOf(entry.label) === fingerprint);
    if (target === undefined) return "no_such_variant";
    if (target === playing.dub) return "done"; // уже играет
    const { seasonDepth, episodeDepth } = roles;
    const { dub, season, episode } = playing;
    if (seasonDepth === undefined || episodeDepth === undefined || dub === undefined || season === undefined || episode === undefined) {
      if (episodeDepth !== undefined) return "no_such_variant"; // серии без сезонов в папках озвучек не видели — не гадаем
      dom.pressLikePerson(target.element);
      return "done";
    }
    // Посреди трёх нажатий меню страницы «на полпути», поэтому описание отвечает тем, что играет, а вторая
    // команда получает «такого нет» (`playing` у неё не устоялось) — нажатия двух смен не перемешаются.
    return switchFolderDub({ view: dom, roles: { dubDepth: roles.dubDepth, seasonDepth, episodeDepth }, playing: { dub, season, episode }, target, pause });
  }

  async function selectQuality(state: PageState, key: string, fingerprint: string): Promise<VariantActionOutcome> {
    const index = key.startsWith(QUALITY_ID_PREFIX) ? Number(key.slice(QUALITY_ID_PREFIX.length)) : Number.NaN;
    const label = Number.isInteger(index) ? state.facts.qualities?.[index] : undefined;
    if (label === undefined || optionFingerprintOf(label) !== fingerprint) return "no_such_variant";
    return handOutcome(await player.call(PLAYERJS_HAND_ACTIONS.selectQuality, String(index)));
  }

  return {
    // Разметка PlayerJS без классов и адресов: признак — свой корень `<pjsdiv>` в документе (как у читателя рекламы).
    matches: (place: AdFramePlace) => place.contains(PLAYERJS_ROOT_SELECTOR),
    // Весь документ: списки плейлиста — соседи, общего корня у них в разметке нет. Шкала времени тоже двигает
    // DOM, но чтения сливаются (не чаще 4 в секунду, `content/variant-watch.ts`), а качество меняется без
    // мутаций разметки — его подхватывает перечитывание на каждом продлении аренды (раз в секунду).
    observationRoot: () => dom.documentRoot(),
    async describe(): Promise<PageVariantDescription | null> {
      const state = await readState();
      if (state === null) {
        lastSettled = null;
        return null;
      }
      // Меню «на полпути» (человек открыл папку или идёт наша смена в три нажатия): отвечаем тем, что играет.
      if (state.playing === undefined) return lastSettled;
      lastSettled = describeState(state, state.playing);
      return lastSettled;
    },
    async select(kind: PageVariantKind, id: string): Promise<VariantActionOutcome> {
      const parsed = parseOptionId(id);
      if (parsed === undefined || kind === "subtitles") return "no_such_variant";
      const state = await readState();
      if (state === null) return "no_such_variant";
      if (kind === "quality") return selectQuality(state, parsed.key, parsed.fingerprint);
      if (PLAYERJS_PLAYLIST_MARKUP.audioTrackPathPattern.test(parsed.key)) return selectAudioTrack(state, parsed.key, parsed.fingerprint);
      return selectFolderDub(state, parsed.key, parsed.fingerprint);
    },
    async next(): Promise<VariantActionOutcome> {
      const state = await readState();
      // Next только по тому, что играет, и только внутри той же озвучки (`hasNextEpisode`).
      if (state === null || state.playing === undefined || !hasNextEpisode(state.reading, state.roles, state.playing)) return "no_such_variant";
      return handOutcome(await player.call(PLAYERJS_HAND_ACTIONS.nextItem, null));
    },
  };
}
