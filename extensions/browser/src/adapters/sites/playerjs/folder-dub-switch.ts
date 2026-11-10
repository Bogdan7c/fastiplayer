// Смена озвучки, которая у сайта — ПАПКА плейлиста (Ashdi с `?multivoice`, сессия 15b).
//
// Нажатие папки озвучки лишь открывает её: играет прежний файл, а списки сезонов и серий перерисованы под
// новую озвучку. Поэтому смена — три нажатия подряд (решение 294): озвучка → сезон с той же подписью →
// серия с той же подписью. Только третье нажатие меняет то, что на экране; секунду возвращает наш плеер
// (решение 280).
//
// Если у новой озвучки нет этого сезона или серии, зритель остаётся, где был (решение 295): видео играет
// как играло, а меню страницы возвращается к прежней озвучке и сезону (папки открываются заново, серия не
// нажимается — нажать играющую серию значит начать её заново).

import type { VariantActionOutcome, VariantPageView } from "../../capabilities/variants/contract.ts";
import { childrenOf, readPlaylist, type PlayingEntries, type PlaylistEntry, type PlaylistRoles } from "./playlist-reading.ts";
import { PLAYERJS_FOLDER_SWITCH_TIMING } from "./variant-markup.ts";

/** Пауза между взглядами на страницу; в тестах — подделка часов. */
export interface PlayerJsPause {
  sleep(milliseconds: number): Promise<void>;
}

/** Что нужно для смены: взгляд, роли уровней, что играет сейчас, куда переключаем. */
export interface FolderDubSwitch {
  readonly view: VariantPageView;
  readonly roles: Required<PlaylistRoles>;
  readonly playing: Required<PlayingEntries>;
  readonly target: PlaylistEntry;
  readonly pause: PlayerJsPause;
}

/**
 * Дождаться, пока в папке `parent` уровня `depth` появятся пункты (страница перерисовала список после
 * нажатия). `undefined` — не дождались за `waitLimitMs`.
 */
async function waitForChildren(view: VariantPageView, depth: number, parent: PlaylistEntry, pause: PlayerJsPause): Promise<readonly PlaylistEntry[] | undefined> {
  const { pollMs, waitLimitMs } = PLAYERJS_FOLDER_SWITCH_TIMING;
  for (let waited = 0; waited <= waitLimitMs; waited += pollMs) {
    const children = childrenOf(readPlaylist(view), depth, parent);
    if (children.length > 0) return children;
    await pause.sleep(pollMs);
  }
  return undefined;
}

/** Открыть папку и найти в ней пункт с подписью `label`: `undefined` — не открылась или такого нет. */
async function openAndFind(input: FolderDubSwitch, folder: PlaylistEntry, childDepth: number, label: string): Promise<PlaylistEntry | "timeout" | undefined> {
  input.view.pressLikePerson(folder.element);
  const children = await waitForChildren(input.view, childDepth, folder, input.pause);
  if (children === undefined) return "timeout";
  return children.find((child) => child.label === label);
}

/**
 * Вернуть меню страницы к играющей озвучке и сезону: открыть их папки снова. Серию не нажимаем. Пункты
 * ищем заново: после перерисовки старые элементы страницы могли смениться новыми с тем же путём.
 */
async function restorePageMenu(input: FolderDubSwitch): Promise<void> {
  const { roles, playing, view } = input;
  const dub = (readPlaylist(view).levels.get(roles.dubDepth) ?? []).find((entry) => entry.path === playing.dub.path);
  if (dub === undefined) return;
  const season = await openAndFind(input, dub, roles.seasonDepth, playing.season.label);
  if (season === undefined || season === "timeout") return;
  input.view.pressLikePerson(season.element);
  await waitForChildren(view, roles.episodeDepth, season, input.pause);
}

/** Три нажатия смены озвучки; при отсутствии серии в новой озвучке — откат меню и «такого нет». */
export async function switchFolderDub(input: FolderDubSwitch): Promise<VariantActionOutcome> {
  const { roles, playing, target } = input;
  const season = await openAndFind(input, target, roles.seasonDepth, playing.season.label);
  if (season === "timeout") {
    await restorePageMenu(input);
    return "failed";
  }
  if (season === undefined) {
    await restorePageMenu(input);
    return "no_such_variant";
  }
  const episode = await openAndFind(input, season, roles.episodeDepth, playing.episode.label);
  if (episode === "timeout" || episode === undefined) {
    await restorePageMenu(input);
    return episode === "timeout" ? "failed" : "no_such_variant";
  }
  input.view.pressLikePerson(episode.element);
  return "done";
}
