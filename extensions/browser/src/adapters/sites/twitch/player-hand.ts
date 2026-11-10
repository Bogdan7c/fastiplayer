// Белый список руки-функции Twitch (MAIN-мир): прочитать меню качества и выбрать качество. Больше ничего.
//
// Плеер ищем заново при каждом действии (страница пересоздаёт его). Страница — враг: любое свойство
// может оказаться геттером-ловушкой, поэтому все чтения защищены, а наружу уходит только копия простых значений.

import { PAGE_FUNCTION_MISSING, pageDocumentOf, type PlayerHandActions } from "../../capabilities/player-hand/contract.ts";
import { TWITCH_HAND_ACTIONS, TWITCH_MARKUP } from "./markup.ts";

const { playerFunctions, autoQuality } = TWITCH_MARKUP;

/** Чтение свойства, которое не падает на геттере-ловушке: что не прочиталось, того нет. */
function safeGet(target: object, key: string): unknown {
  try {
    return Reflect.get(target, key);
  } catch {
    return undefined;
  }
}

function isObject(candidate: unknown): candidate is object {
  return typeof candidate === "object" && candidate !== null;
}

/** Плеер, если у узла цепочки в свойствах компонента есть `mediaPlayerInstance` с функцией `getQualities`. */
function playerOfFiberNode(node: object): object | undefined {
  const props = safeGet(node, "memoizedProps");
  if (!isObject(props)) return undefined;
  const player = safeGet(props, TWITCH_MARKUP.playerPropertyName);
  if (!isObject(player)) return undefined;
  return typeof safeGet(player, TWITCH_MARKUP.playerSignatureFunction) === "function" ? player : undefined;
}

/** Найти плеер страницы: элемент `.video-player` → его fiber → вверх по `return` не дальше `maxFiberNodes` узлов. */
export function findTwitchPlayer(page: object): object | undefined {
  const element = pageDocumentOf(page)?.querySelector(TWITCH_MARKUP.playerElementSelector);
  if (!isObject(element)) return undefined;
  const fiberKey = Object.keys(element).find((key) => key.startsWith(TWITCH_MARKUP.fiberKeyPrefix));
  if (fiberKey === undefined) return undefined;
  let node: unknown = safeGet(element, fiberKey);
  for (let step = 0; step < TWITCH_MARKUP.maxFiberNodes && isObject(node); step += 1) {
    const player = playerOfFiberNode(node);
    if (player !== undefined) return player;
    node = safeGet(node, "return");
  }
  return undefined;
}

/** Вызов функции плеера с его `this`; нет функции — `undefined`. Исключения функции летят наверх (их ловит исполнитель). */
function callPlayer(player: object, name: string, ...functionArguments: readonly unknown[]): unknown {
  const method = safeGet(player, name);
  return typeof method === "function" ? Reflect.apply(method, player, functionArguments) : undefined;
}

/** Группа качества из объекта плеера: непустая строка или `undefined`. */
function groupOf(quality: unknown): string | undefined {
  const group = isObject(quality) ? safeGet(quality, "group") : undefined;
  return typeof group === "string" && group !== "" ? group : undefined;
}

/** Качества плеера как массив его же объектов; не массив — срыв (полузнание хуже незнания). */
function qualitiesOf(player: object): readonly unknown[] {
  const qualities = callPlayer(player, playerFunctions.qualities);
  if (!Array.isArray(qualities)) throw new Error("getQualities is not a list");
  return qualities;
}

/** Описание: ключ эфира (логин канала или `null`) и пункты качества — «Auto» первым, остальные по `getQualities()`. */
function describePlayer(player: object): unknown {
  const autoMode = callPlayer(player, playerFunctions.isAutoMode) === true;
  const currentGroup = groupOf(callPlayer(player, playerFunctions.currentQuality));
  const quality: { id: string; label: string; selected: boolean; automatic?: boolean }[] = [
    { id: autoQuality.id, label: autoQuality.label, selected: autoMode, automatic: true },
  ];
  for (const candidate of qualitiesOf(player)) {
    const group = groupOf(candidate);
    const name = isObject(candidate) ? safeGet(candidate, "name") : undefined;
    // Группа с пробелом, кириллицей или слишком длинная не годится в ключ протокола — пункт пропускаем.
    if (group === undefined || typeof name !== "string" || !TWITCH_MARKUP.optionIdPattern.test(group)) continue;
    if (group === autoQuality.id) continue; // чужой пункт не должен притвориться нашим «Auto»
    quality.push({ id: group, label: name, selected: !autoMode && group === currentGroup });
  }
  const login = callPlayer(player, playerFunctions.channelLogin);
  // Ни одного годного качества (плеер ещё строится) — меню нет: одинокое «Auto» зрителю ничего не даёт.
  const menu = quality.length > 1 ? quality : [];
  return { episodeKey: typeof login === "string" && login !== "" ? login : null, quality: menu };
}

/** Выбор: `auto` включает автоматику, иначе ищем в списке плеера ОБЪЕКТ с такой группой (`setQuality` ждёт объект). */
function selectQuality(player: object, id: string): boolean {
  if (id === autoQuality.id) {
    callPlayer(player, playerFunctions.setAutoMode, true);
    return true;
  }
  const target = qualitiesOf(player).find((candidate) => groupOf(candidate) === id);
  if (target === undefined) return false;
  callPlayer(player, playerFunctions.setQuality, target);
  return true;
}

export const TWITCH_PLAYER_HAND: PlayerHandActions = {
  [TWITCH_HAND_ACTIONS.describe]: (page) => {
    const player = findTwitchPlayer(page);
    return player === undefined ? PAGE_FUNCTION_MISSING : describePlayer(player);
  },
  [TWITCH_HAND_ACTIONS.selectQuality]: (page, argument) => {
    const player = findTwitchPlayer(page);
    if (player === undefined) return PAGE_FUNCTION_MISSING;
    return argument === null ? false : selectQuality(player, argument);
  },
};
