// Белый список руки-функции адаптера стенда (MAIN-мир): четыре функции плеера страницы и ничего больше.

import { callPageFunction, type PlayerHandActions } from "../../capabilities/player-hand/contract.ts";
import { BENCH_SERIES_HAND_ACTIONS, BENCH_SERIES_MARKUP } from "./markup.ts";

const { playerObject, playerFunctions } = BENCH_SERIES_MARKUP;

export const BENCH_SERIES_PLAYER_HAND: PlayerHandActions = {
  [BENCH_SERIES_HAND_ACTIONS.qualities]: (page) => callPageFunction(page, playerObject, playerFunctions.qualities),
  [BENCH_SERIES_HAND_ACTIONS.selectQuality]: (page, argument) => callPageFunction(page, playerObject, playerFunctions.selectQuality, argument),
  [BENCH_SERIES_HAND_ACTIONS.hasNextEpisode]: (page) => callPageFunction(page, playerObject, playerFunctions.hasNextEpisode),
  [BENCH_SERIES_HAND_ACTIONS.nextEpisode]: (page) => callPageFunction(page, playerObject, playerFunctions.nextEpisode),
};
