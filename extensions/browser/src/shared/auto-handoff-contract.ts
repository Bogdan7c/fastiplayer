// Договор режима 2 «автозапуск» (сессия 12a, решения 246–254): кто кому что говорит.
//
// Три участка, как у сторожа следующего видео (09c2):
// 1. изолированный мир → MAIN-мир (личный порт): «охрана включена/выключена» и «кредит клика»;
// 2. фон → изолированный мир (широковещательный `tabs.sendMessage`): «кредит клика» и «отпусти задержанное»;
// 3. MAIN-мир → фон: обычное донесение `next_video_held` с `trigger: "viewer_play"` (handoff-contract.ts).
//
// Правил сайтов, стоп-листа и порога длины MAIN-мир не знает: он только честно говорит «человек сам запустил
// видео», а судит фон (фильтры звука и цикла охрана знает только как общие включатели, без списков сайтов). Поэтому страница не может ни включить режим 2 сама, ни узнать правила.

import type { ElementKey } from "./handoff-contract.ts";

// ---------------------------------------------------------------------------------------------
// Участок 1: изолированный мир → MAIN-мир (личный порт участка 1).
// ---------------------------------------------------------------------------------------------

/**
 * Охрана режима 2 в этом документе:
 * - `on` — настройка включена хотя бы для какого-то сайта: сторож задерживает видео, которое человек сам
 *   запустил, и спрашивает фон;
 * - `off` — режим выключен: сторож строго прозрачен для `play()` вне взвода фона, фону не уходит ничего.
 */
export type AutoHandoffWatchState = "on" | "off";

/** Что охрана делает с таким видео сама: `skip` — пропускает мимо (фильтр включён везде), `hold` — задерживает и спрашивает фон. */
export type WatchFilterAction = "skip" | "hold";

export const AUTO_HANDOFF_WATCH_STATES: readonly AutoHandoffWatchState[] = ["on", "off"];
export const WATCH_FILTER_ACTIONS: readonly WatchFilterAction[] = ["skip", "hold"];

/**
 * Охрана целиком (решения 260–261): состояние и что делать с видео без звука и с зацикленным видео.
 * Изолированный мир шлёт это на старте документа и при каждом изменении настройки.
 */
export interface AutoHandoffWatch {
  readonly state: AutoHandoffWatchState;
  readonly muted: WatchFilterAction;
  readonly looped: WatchFilterAction;
}

/** Охрана выключена (значение до первой команды и при выключенном режиме). */
export const AUTO_HANDOFF_WATCH_OFF: AutoHandoffWatch = { state: "off", muted: "skip", looped: "skip" };

export interface AutoHandoffWatchCommand extends AutoHandoffWatch {
  readonly kind: "auto_handoff_watch";
}

/**
 * «Кредит клика» (решение 247): человек нажал play на коротком ролике (реклама перед фильмом), и следующий ролик
 * сайт запустит сам, без клика. Пока кредит жив, запуск видео со звуком в этом документе считается запуском
 * человека. Срок вместо команды «снять»: фон может умереть, а кредит не должен жить вечно.
 */
export interface GrantPlayCreditCommand {
  readonly kind: "grant_play_credit";
  readonly lease_ms: number;
}

/** Все команды режима 2, которые MAIN-мир получает по личному порту. */
export type AutoHandoffPageCommand = AutoHandoffWatchCommand | GrantPlayCreditCommand;

/**
 * Предел срока кредита клика, мс. Кредит выдаётся на длину короткого ролика плюс запас: ролик короче порога
 * режима 2 (по умолчанию 60 с, но порог настраивается до `MAX_AUTO_HANDOFF_MIN_DURATION_S`), а цепочка реклам
 * продлевает кредит каждым роликом заново. Десять минут — с запасом на длинный порог, но не «навсегда».
 */
export const MAX_PLAY_CREDIT_LEASE_MS = 600_000;

// ---------------------------------------------------------------------------------------------
// Участок 2: фон → изолированный мир (`tabs.sendMessage`; фон — единственный отправитель).
// ---------------------------------------------------------------------------------------------

/** Вид просьбы фона всем фреймам вкладки: «выдай кредит клика». */
export const GRANT_PLAY_CREDIT_KIND = "grant_play_credit";

export interface GrantPlayCreditRequest {
  readonly kind: typeof GRANT_PLAY_CREDIT_KIND;
  readonly lease_ms: number;
}

/** Вид просьбы фона одному фрейму: «отпусти задержанное видео» (сеанс режима 2 не состоялся). */
export const RELEASE_HELD_VIDEO_REQUEST_KIND = "release_held_video_request";

export interface ReleaseHeldVideoRequest {
  readonly kind: typeof RELEASE_HELD_VIDEO_REQUEST_KIND;
  readonly element: ElementKey;
}
