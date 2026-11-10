// Подделка страницы PlayerJS по фактам разведки 15b (Tortuga на uaserials, Ashdi на klonua): разметка
// плейлиста `<pjsdiv me="…">` и `window.pljssglobal[0].api(…)`. Подделка ведёт себя как настоящий плеер:
// - Tortuga: сезоны → серии, озвучки — дорожки звука `audio_N`; нажатие сезона сразу играет его 1-ю серию;
// - Ashdi `?multivoice`: озвучки → сезоны → серии; нажатие озвучки или сезона только ОТКРЫВАЕТ папку
//   (заголовок «...»), играет лишь нажатая серия; `playlist_id` — id файла;
// - ровный список (фильм Ashdi или незнакомый сайт).
// Перерисовка после нажатия откладывается до следующей паузы адаптера (`flush`): так настоящая страница
// перерисовывает списки не мгновенно, и адаптер обязан дождаться.
import { FakeNode } from "./fake-ad-dom.ts";

/** Одна серия: подпись и id файла (у Ashdi лист пути несёт его). */
export interface FakeEpisode {
  readonly label: string;
  readonly fileId: number;
}

export interface FakeSeason {
  readonly label: string;
  readonly episodes: readonly FakeEpisode[];
}

export interface FakeDub {
  readonly label: string;
  readonly seasons: readonly FakeSeason[];
}

/** Что страница сделала по просьбам `api()` и нажатиям — для проверок «не тронул ли лишнего». */
export interface FakePlayerLog {
  readonly apiCalls: string[];
}

/** Общий плеер подделки: `api()` и окно страницы. */
interface FakeApiState {
  qualities: unknown;
  quality: unknown;
  ready: boolean;
}

const ACTIVE_CLASS = "pjs-videoplayer42-active-pl";

function item(path: string, label: string, active: boolean): FakeNode {
  return new FakeNode("pjsdiv", { text: label, attributes: active ? { me: path, class: ACTIVE_CLASS } : { me: path } });
}

/** Каркас фрейма плеера: корень `pjsdiv`, видео, список для пунктов. */
function frame(): { root: FakeNode; lists: FakeNode } {
  const root = new FakeNode("html");
  const player = new FakeNode("pjsdiv", { id: "oframevideoplayer42" });
  const lists = new FakeNode("pjsdiv");
  root.add(player.add(new FakeNode("video"), lists));
  return { root, lists };
}

export interface FakePlayerJsPage {
  readonly root: FakeNode;
  readonly window: object;
  readonly log: FakePlayerLog;
  readonly api: FakeApiState;
  /** Применить отложенную перерисовку (настоящая страница рисует списки чуть позже нажатия). */
  flush(): void;
}

/** Окно страницы с `pljssglobal[0].api` поверх обработчика команд. */
function pageWindow(address: { pathname: string; search: string }, apiState: FakeApiState, log: FakePlayerLog, handle: (command: string, parameter: unknown) => unknown): object {
  return {
    location: address,
    pljssglobal: [
      {
        api(command: string, parameter?: unknown) {
          log.apiCalls.push(parameter === undefined ? command : `${command}:${String(parameter)}`);
          if (!apiState.ready) return false;
          if (command === "qualities") return apiState.qualities;
          if (command === "quality") {
            if (parameter === undefined) return apiState.quality;
            const list = apiState.qualities as string[];
            apiState.quality = list[Number(parameter)];
            return undefined;
          }
          return handle(command, parameter);
        },
      },
    ],
  };
}

/** Tortuga: сезоны и серии, озвучки — дорожки звука. */
export function tortugaPage(options: { seasons: readonly FakeSeason[]; dubs: readonly string[]; playing?: { season: number; episode: number } }): FakePlayerJsPage & { state: { season: number; episode: number; dub: number } } {
  const { root, lists } = frame();
  const log: FakePlayerLog = { apiCalls: [] };
  const api: FakeApiState = { qualities: ["480p", "720p", "1080p"], quality: "1080p", ready: true };
  const state = { season: options.playing?.season ?? 0, episode: options.playing?.episode ?? 0, dub: 0 };
  let pending = false;
  function render(): void {
    lists.children.length = 0;
    const season = options.seasons[state.season];
    const episode = season?.episodes[state.episode];
    const seasonList = new FakeNode("pjsdiv").add(item(`head_x-${state.season}`, season?.label ?? "", false));
    options.seasons.forEach((candidate, index) => {
      const node = item(`x-${index}`, candidate.label, index === state.season);
      // Нажатие сезона у Tortuga сразу играет его первую серию.
      node.onPress = () => {
        state.season = index;
        state.episode = 0;
        pending = true;
      };
      seasonList.add(node);
    });
    const episodeList = new FakeNode("pjsdiv").add(item(`head_xx-${state.season}-${state.episode}-1-${state.episode + 1}`, episode?.label ?? "", false));
    season?.episodes.forEach((candidate, index) => {
      const node = item(`xx-${state.season}-${index}-1-${index + 1}`, candidate.label, index === state.episode);
      node.onPress = () => {
        state.episode = index;
        pending = true;
      };
      episodeList.add(node);
    });
    const audioList = new FakeNode("pjsdiv").add(item(`head_audio-${state.dub}`, options.dubs[state.dub] ?? "", false));
    options.dubs.forEach((label, index) => audioList.add(item(`audio_${index}`, label, index === state.dub)));
    lists.add(episodeList, seasonList, audioList);
  }
  render();
  const window = pageWindow({ pathname: "/embed/869", search: "" }, api, log, (command, parameter) => {
    if (command === "playlist_id") return `${state.season + 1}-${state.episode + 1}`;
    if (command === "audiotracks") return [...options.dubs];
    if (command === "audiotrack") {
      state.dub = Number(parameter);
      render();
      return undefined;
    }
    if (command === "next") {
      const season = options.seasons[state.season];
      if (season !== undefined && state.episode + 1 < season.episodes.length) state.episode += 1;
      else if (state.season + 1 < options.seasons.length) {
        state.season += 1;
        state.episode = 0;
      }
      render();
      return undefined;
    }
    return undefined;
  });
  return {
    root,
    window,
    log,
    api,
    state,
    flush: () => {
      if (pending) render();
      pending = false;
    },
  };
}

/** Ashdi `?multivoice`: озвучки → сезоны → серии; папки только открываются, играет нажатая серия. */
export function ashdiSeriesPage(options: { dubs: readonly FakeDub[]; playing: { dub: number; season: number; episode: number }; search?: string }) {
  const { root, lists } = frame();
  const log: FakePlayerLog = { apiCalls: [] };
  const api: FakeApiState = { qualities: ["480p", "720p", "1080p", "Авто"], quality: "480p", ready: true };
  const playing = { ...options.playing };
  /** Открытые папки: озвучка всегда, сезон — `null`, пока в открытой озвучке сезон не нажат. */
  const open: { dub: number; season: number | null } = { dub: playing.dub, season: playing.season };
  let pending = false;
  const presses: string[] = [];
  const playingFile = (): FakeEpisode | undefined => options.dubs[playing.dub]?.seasons[playing.season]?.episodes[playing.episode];
  function render(): void {
    lists.children.length = 0;
    const dubList = new FakeNode("pjsdiv").add(item(`head_x-${open.dub}`, options.dubs[open.dub]?.label ?? "", false));
    options.dubs.forEach((dub, index) => {
      const node = item(`x-${index}`, ` ${dub.label}`, index === open.dub);
      node.onPress = () => {
        presses.push(`dub:${dub.label}`);
        open.dub = index;
        open.season = null;
        pending = true;
      };
      dubList.add(node);
    });
    lists.add(dubList);
    const seasons = options.dubs[open.dub]?.seasons ?? [];
    const seasonList = new FakeNode("pjsdiv").add(item(`head_xx-${open.dub}-${open.season ?? 0}`, open.season === null ? "..." : (seasons[open.season]?.label ?? ""), false));
    seasons.forEach((season, index) => {
      const node = item(`xx-${open.dub}-${index}`, season.label, index === open.season);
      node.onPress = () => {
        presses.push(`season:${season.label}`);
        open.season = index;
        pending = true;
      };
      seasonList.add(node);
    });
    lists.add(seasonList);
    if (open.season === null) return; // серий не видно, пока сезон открытой озвучки не выбран
    const openSeason = open.season;
    const episodes = seasons[openSeason]?.episodes ?? [];
    const playingHere = open.dub === playing.dub && openSeason === playing.season;
    const episodeList = new FakeNode("pjsdiv").add(item("head_xxx", playingHere ? (playingFile()?.label ?? "") : "...", false));
    episodes.forEach((episode, index) => {
      const node = item(`xxx-${open.dub}-${openSeason}-${index}-${episode.fileId}`, episode.label, playingHere && index === playing.episode);
      node.onPress = () => {
        presses.push(`episode:${episode.label}`);
        playing.dub = open.dub;
        playing.season = openSeason;
        playing.episode = index;
        pending = true;
      };
      episodeList.add(node);
    });
    lists.add(episodeList);
  }
  render();
  const window = pageWindow({ pathname: "/serial/6320", search: options.search ?? "?multivoice" }, api, log, (command) => {
    if (command === "playlist_id") return String(playingFile()?.fileId ?? "");
    if (command === "audiotracks") return [];
    if (command === "next") {
      const season = options.dubs[playing.dub]?.seasons[playing.season];
      if (season !== undefined && playing.episode + 1 < season.episodes.length) playing.episode += 1;
      else {
        playing.season += 1;
        playing.episode = 0;
      }
      open.dub = playing.dub;
      open.season = playing.season;
      render();
    }
    return undefined;
  });
  return {
    root,
    window,
    log,
    api,
    playing,
    open,
    presses,
    /** Человек сам открыл на странице папку другой озвучки (меню «на полпути»). */
    openDubFolderByHand(index: number): void {
      open.dub = index;
      open.season = null;
      render();
    },
    flush: () => {
      if (pending) render();
      pending = false;
    },
  };
}

/** Ровный список: фильм Ashdi с озвучками или серии незнакомого сайта. `fileIds` — у Ashdi в конце пути. */
export function flatPlaylistPage(options: { labels: readonly string[]; playing: number; fileIds?: readonly number[]; pathname: string; search: string }) {
  const { root, lists } = frame();
  const log: FakePlayerLog = { apiCalls: [] };
  const api: FakeApiState = { qualities: ["480p", "720p"], quality: "720p", ready: true };
  const state = { playing: options.playing };
  let pending = false;
  const pathOf = (index: number): string => (options.fileIds === undefined ? `x-${index}` : `x-${index}-${options.fileIds[index]}`);
  function render(): void {
    lists.children.length = 0;
    const list = new FakeNode("pjsdiv").add(item(`head_${pathOf(state.playing)}`, options.labels[state.playing] ?? "", false));
    options.labels.forEach((label, index) => {
      const node = item(pathOf(index), label, index === state.playing);
      node.onPress = () => {
        state.playing = index;
        pending = true;
      };
      list.add(node);
    });
    lists.add(list);
  }
  render();
  const window = pageWindow({ pathname: options.pathname, search: options.search }, api, log, (command) => {
    if (command === "playlist_id") return options.fileIds === undefined ? String(state.playing) : String(options.fileIds[state.playing]);
    if (command === "audiotracks") return [];
    if (command === "next") {
      state.playing += 1;
      render();
    }
    return undefined;
  });
  return {
    root,
    window,
    log,
    api,
    state,
    flush: () => {
      if (pending) render();
      pending = false;
    },
  };
}

/** Плеер без плейлиста (uakino): только качества. */
export function qualityOnlyPage(options: { qualities: unknown; quality: unknown; ready?: boolean }) {
  const { root } = frame();
  const log: FakePlayerLog = { apiCalls: [] };
  const api: FakeApiState = { qualities: options.qualities, quality: options.quality, ready: options.ready ?? true };
  const window = pageWindow({ pathname: "/vod/300629", search: "" }, api, log, (command) => (command === "playlist_id" ? undefined : command === "audiotracks" ? [] : undefined));
  return { root, window, log, api, flush: () => undefined };
}
