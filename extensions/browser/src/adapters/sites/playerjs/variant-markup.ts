// Данные адаптера вариантов PlayerJS (сессия 15b): разметка плейлиста, функции `api()`, профили хостов.
// Логика их не знает наизусть: сайт сменил разметку — правим этот файл.
//
// Факты разведки 15b (невидимый Chromium, uaserials/Tortuga, klonua/Ashdi, 10 ноября 2026):
// - плейлист PlayerJS в виде выпадающих списков: каждый пункт — `<pjsdiv me="…">`; заголовок списка —
//   `me="head_…"` с подписью текущего пункта (или «...», пока папка открыта, а пункт в ней не выбран);
//   выбранный пункт помечен классом `pjs-<id плеера>-active-pl`;
// - `me` пункта плейлиста — путь по дереву: `x-<i>` (1-й уровень), `xx-<i>-<j>` (2-й), `xxx-<i>-<j>-<k>-<id файла>`
//   (3-й, Ashdi). Число `x` — глубина. Дорожки звука PlayerJS — отдельный список `me="audio_<номер>"` (Tortuga);
// - Ashdi с `?multivoice`: 1-й уровень — озвучки, 2-й — сезоны, 3-й — серии (у фильма — только озвучки);
//   нажатие озвучки или сезона лишь ОТКРЫВАЕТ папку, играть начинает только серия;
// - Tortuga: 1-й уровень — сезоны, 2-й — серии; нажатие сезона сразу играет его первую серию;
// - `element.click()` пункт не принимает, принимает «нажатие как человек» (цепочка событий мыши);
// - `api("audiotrack", n)` меняет озвучку Tortuga на той же серии; `api("next")` — следующий файл плейлиста.

/** Разметка плейлиста PlayerJS. */
export const PLAYERJS_PLAYLIST_MARKUP = {
  /** Пункт плейлиста или заголовок списка: элемент с атрибутом пути. */
  itemSelector: "pjsdiv[me]",
  /** Атрибут пути пункта в дереве плейлиста. */
  pathAttribute: "me",
  /** Атрибут классов: выбранный пункт узнаём по суффиксу класса (имя плеера в классе у каждого сайта своё). */
  classAttribute: "class",
  /** Суффикс класса выбранного пункта: `pjs-videoplayer2612-active-pl`. */
  activeClassSuffix: "-active-pl",
  /** Приставка `me` заголовка списка. */
  headPrefix: "head_",
  /** Подпись заголовка, пока в открытой папке ничего не выбрано: меню страницы «на полпути». */
  pendingHeadLabel: "...",
  /** Пункт дорожки звука PlayerJS: `audio_<номер>`. */
  audioTrackPathPattern: /^audio_(\d{1,3})$/u,
  /** Пункт плейлиста: `x…x-<числа через дефис>`; число `x` — глубина уровня. */
  playlistPathPattern: /^(x{1,6})-(\d{1,12}(?:-\d{1,12}){0,8})$/u,
} as const;

/** Корень разметки PlayerJS: по нему адаптер узнаёт фрейм с этим плеером (как читатель рекламы 09d). */
export const PLAYERJS_ROOT_SELECTOR = "pjsdiv";

/** Подписи автоматического качества («Авто» у Ashdi): такой пункт память выбора не пишет (решение 284). */
export const PLAYERJS_AUTOMATIC_QUALITY_LABELS: readonly string[] = ["авто", "auto", "автоматично", "автоматически"];

/**
 * На незнакомом сайте (решение 296, «осторожно») ровный список считаем сериями, только если подписи
 * похожи на серии: «Серія 5», «Episode 5», «5 серія». Иначе Next не предлагаем.
 */
export const PLAYERJS_EPISODE_LABEL_PATTERN = /^(?:(?:серія|серия|епізод|эпизод|episode|ep\.?)\s*\d{1,4}|\d{1,4}\s*(?:серія|серия|епізод|эпизод|episode))(?:\b|$)/iu;

/**
 * Профиль хоста: что значат уровни плейлиста на проверенном сайте (решение 296).
 * - `dubFolderLevel` — уровень, где папки — озвучки; только при признаке в адресе фрейма (`?multivoice` у Ashdi).
 * - `leafCarriesFileId` — последнее число `me` листа — id файла, равный `api("playlist_id")`: так видно,
 *   какой пункт ИГРАЕТ, а не просто открыт.
 * - `episodesTrusted` — нижний уровень плейлиста — серии без проверки подписей.
 */
export interface PlayerJsHostProfile {
  readonly hostSuffixes: readonly string[];
  readonly dubFolderLevel?: { readonly depth: number; readonly addressFlag: string };
  readonly leafCarriesFileId: boolean;
  readonly episodesTrusted: boolean;
}

export const PLAYERJS_HOST_PROFILES: readonly PlayerJsHostProfile[] = [
  { hostSuffixes: ["ashdi.vip"], dubFolderLevel: { depth: 1, addressFlag: "multivoice" }, leafCarriesFileId: true, episodesTrusted: true },
  { hostSuffixes: ["tortuga.tw"], leafCarriesFileId: false, episodesTrusted: true },
];

/** Профиль незнакомого сайта с PlayerJS: папки — не озвучки, серии — только по подписям. */
export const PLAYERJS_UNKNOWN_HOST_PROFILE: PlayerJsHostProfile = { hostSuffixes: [], leafCarriesFileId: false, episodesTrusted: false };

/** Имена действий руки-функции PlayerJS (белый список `player-hand.ts`). */
export const PLAYERJS_HAND_ACTIONS = {
  describe: "describe_player",
  selectQuality: "select_quality",
  selectAudioTrack: "select_audio_track",
  nextItem: "next_item",
} as const;

/** Функции и запросы PlayerJS (документация playerjs.com, «API commands»). */
export const PLAYERJS_API = {
  /** Глобальный список экземпляров плеера во фрейме: у каждого есть `api(команда, параметр)`. */
  instancesGlobal: "pljssglobal",
  apiFunction: "api",
  qualities: "qualities",
  /** Без параметра — подпись текущего качества; с номером — выбрать. */
  quality: "quality",
  /** Подписи дорожек звука (озвучки Tortuga); у фильма с одной дорожкой бывает пустой список. */
  audioTracks: "audiotracks",
  /** С номером — включить дорожку звука (PlayerJS ≥ 9.7). */
  audioTrack: "audiotrack",
  playlistId: "playlist_id",
  next: "next",
} as const;

/** Сколько ждать перестройки списков после нажатия папки и как часто смотреть (Ashdi, три нажатия). */
export const PLAYERJS_FOLDER_SWITCH_TIMING = { pollMs: 100, waitLimitMs: 3_000 } as const;

/** Предел длины строк, которые рука-функция отдаёт из окна страницы. */
export const PLAYERJS_MAX_FIELD_LENGTH = 512;
