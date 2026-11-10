// Белый список руки-функции PlayerJS (MAIN-мир, сессия 15b): четыре действия над `api()` плеера и ничего больше.
//
// Плеер ищем заново при каждом действии: страница вправе его пересоздать. Страница — враг: любое свойство
// может оказаться геттером-ловушкой, поэтому чтения защищены, а наружу уходят только строки, числа и
// списки строк. Исключения самой `api()` летят наверх — их ловит исполнитель (`src/page/adapter-hand.ts`).

import { PAGE_FUNCTION_MISSING, type PlayerHandActions } from "../../capabilities/player-hand/contract.ts";
import { PLAYERJS_API as api, PLAYERJS_HAND_ACTIONS, PLAYERJS_MAX_FIELD_LENGTH } from "./variant-markup.ts";

/** Номер пункта в аргументе руки: до трёх цифр (качеств и дорожек у плеера единицы). */
const INDEX_ARGUMENT_PATTERN = /^\d{1,3}$/u;

/** Чтение свойства, которое не падает на геттере-ловушке: что не прочиталось, того нет. */
function safeGet(target: unknown, key: string): unknown {
  if ((typeof target !== "object" && typeof target !== "function") || target === null) return undefined;
  try {
    return Reflect.get(target, key);
  } catch {
    return undefined;
  }
}

/** Строка не длиннее предела руки; не строка — `null`. */
function clippedString(candidate: unknown): string | null {
  return typeof candidate === "string" ? candidate.slice(0, PLAYERJS_MAX_FIELD_LENGTH) : null;
}

/** Список строк из ответа `api()`; не массив строк (у Ashdi без выбора качества бывает `[1]`) — `null`. */
function stringListOf(candidate: unknown): string[] | null {
  if (!Array.isArray(candidate) || !candidate.every((item) => typeof item === "string")) return null;
  return (candidate as string[]).map((item) => item.slice(0, PLAYERJS_MAX_FIELD_LENGTH));
}

/** Плеер фрейма: первый экземпляр из `window.pljssglobal` с функцией `api`. Нет — `undefined`. */
function findPlayer(page: object): object | undefined {
  const instances = safeGet(page, api.instancesGlobal);
  if (!Array.isArray(instances)) return undefined;
  return (instances as unknown[]).find((instance): instance is object => typeof safeGet(instance, api.apiFunction) === "function");
}

/** Вызов `api(команда, …)` с `this` = плеер. */
function callApi(player: object, command: string, ...parameters: readonly unknown[]): unknown {
  const apiFunction = safeGet(player, api.apiFunction) as (...callArguments: unknown[]) => unknown;
  return Reflect.apply(apiFunction, player, [command, ...parameters]);
}

/** Номер из аргумента руки; негодный — `undefined`. */
function indexOf(argument: string | null): number | undefined {
  return argument !== null && INDEX_ARGUMENT_PATTERN.test(argument) ? Number(argument) : undefined;
}

/**
 * Описание плеера одним вызовом. `ready: false` — плеер ещё не построен: до первого клика по видео PlayerJS
 * на все запросы отвечает `false` (разведка uakino). Адрес фрейма — ключ сериала и признак `?multivoice`.
 */
function describePlayer(page: object, player: object): unknown {
  const qualities = callApi(player, api.qualities);
  const quality = callApi(player, api.quality);
  const location = safeGet(page, "location");
  return {
    ready: qualities !== false && quality !== false,
    playlistId: clippedString(callApi(player, api.playlistId)),
    qualities: stringListOf(qualities),
    quality: clippedString(quality),
    pathname: clippedString(safeGet(location, "pathname")) ?? "",
    search: clippedString(safeGet(location, "search")) ?? "",
  };
}

/** Выбрать качество по номеру; номер вне списка качеств — `false` («такого нет»). */
function selectQuality(player: object, argument: string | null): boolean {
  const index = indexOf(argument);
  const qualities = stringListOf(callApi(player, api.qualities));
  if (index === undefined || qualities === null || index >= qualities.length) return false;
  // PlayerJS отвечает `undefined`: проверку «встало ли» делает следующее описание (`api("quality")`).
  callApi(player, api.quality, index);
  return true;
}

/** Включить дорожку звука по номеру (озвучки Tortuga); номера нет в списке дорожек — `false`. */
function selectAudioTrack(player: object, argument: string | null): boolean {
  const index = indexOf(argument);
  const tracks = stringListOf(callApi(player, api.audioTracks));
  if (index === undefined || tracks === null || index >= tracks.length) return false;
  callApi(player, api.audioTrack, index);
  return true;
}

/** Действие над найденным плеером; плеера нет — `PAGE_FUNCTION_MISSING` (страница не та или не готова). */
function withPlayer(page: object, action: (player: object) => unknown): unknown {
  const player = findPlayer(page);
  return player === undefined ? PAGE_FUNCTION_MISSING : action(player);
}

export const PLAYERJS_PLAYER_HAND: PlayerHandActions = {
  [PLAYERJS_HAND_ACTIONS.describe]: (page) => withPlayer(page, (player) => describePlayer(page, player)),
  [PLAYERJS_HAND_ACTIONS.selectQuality]: (page, argument) => withPlayer(page, (player) => selectQuality(player, argument)),
  [PLAYERJS_HAND_ACTIONS.selectAudioTrack]: (page, argument) => withPlayer(page, (player) => selectAudioTrack(player, argument)),
  [PLAYERJS_HAND_ACTIONS.nextItem]: (page) =>
    withPlayer(page, (player) => {
      callApi(player, api.next);
      return true;
    }),
};
