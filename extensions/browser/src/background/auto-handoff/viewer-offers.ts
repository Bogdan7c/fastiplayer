// Предложения «Ask» режима 2 на вкладках (сессия 12b, решения 255–258; договор — src/shared/viewer-offer-contract.ts).
//
// Владеет одним знанием: «на какой вкладке висит предложение и над каким видео». Решает, что делать с выбором
// человека, но саму передачу не ведёт и хранилище не держит — для этого зависимости.
//
// Правила:
// - на вкладке одно предложение: новое (человек нажал play на другом видео той же вкладки) снимает прежнее,
//   прежнее видео остаётся на паузе (`withdrawOffer`). Иначе две панели и два «задержанных» видео сразу;
// - выбор принимается, если фон помнит это предложение ИЛИ сайт вкладки сейчас и правда `ask`. Второе нужно,
//   потому что фон MV3 может уснуть, пока предложение висит (решение 256: оно висит, пока человек не выберет),
//   и память фона пропадёт. Сайт вкладки фон берёт из адреса вкладки, а не из сообщения: правило пишется на
//   сайт, где человек был, и страница этого подделать не может;
// - `always` / `never` пишут правило сайта и сразу действуют (решение 255). Правило, найденное у родителя
//   (`example.com` для `video.example.com`), меняется там же, где записано, как в попапе. Сбой записи действия
//   не отменяет: человек нажал «смотреть», пусть смотрит, а сбой уходит в журнал разработчика.

import type { AutoHandoffSettings, SiteRule } from "../../shared/auto-handoff-settings.ts";
import { withSiteRuleChoice } from "../../shared/auto-handoff-settings-edits.ts";
import type { VideoRef } from "../../shared/handoff-contract.ts";
import type { ViewerOfferAnswer, ViewerOfferChoice, ViewerOfferChoiceMessage } from "../../shared/viewer-offer-contract.ts";
import { sitePolicyOf, siteRuleEntryFor } from "./site-policy.ts";
import { parseWebAddress } from "./stop-list.ts";

/** Что сделать по выбору: правило сайта (если оно меняется) и действие с видео. */
export interface ViewerChoicePlan {
  /** Правило, которое пишется на сайт вкладки; `undefined` — правило не трогаем. */
  readonly rule: SiteRule | undefined;
  readonly action: "hand_off" | "play_here";
}

/** Чистое правило «кнопка → план» (решение 255: «Always / Never» — правило и сразу действие). */
export function planOfViewerChoice(choice: ViewerOfferChoice): ViewerChoicePlan {
  switch (choice) {
    case "fastiplayer":
      return { rule: undefined, action: "hand_off" };
    case "here":
      return { rule: undefined, action: "play_here" };
    case "always":
      return { rule: "always", action: "hand_off" };
    case "never":
      return { rule: "never", action: "play_here" };
  }
}

export interface ViewerOffersDependencies {
  /** Свежая настройка из хранилища (может отклониться). */
  readonly readSettings: () => Promise<AutoHandoffSettings>;
  /** Записать настройку целиком (может отклониться). */
  readonly writeSettings: (settings: AutoHandoffSettings) => Promise<void>;
  /** Адрес вкладки; `undefined` — вкладки уже нет или адрес не отдан. */
  readonly describeTab: (tabId: number) => Promise<{ readonly url?: string | undefined } | undefined>;
  /** Начать обычную передачу режима 2 с выбранным видео (её исход и отпускание видео ведёт вызывающий). */
  readonly startHandoff: (tabId: number, frameId: number, ref: VideoRef) => void;
  /** Просьба одному фрейму снять предложение: видео остаётся на паузе. Сбой доставки не бросает. */
  readonly withdrawOffer: (tabId: number, frameId: number, element: number) => void;
  readonly note: (message: string, error: unknown) => void;
}

export interface ViewerOffers {
  /** На вкладке встало предложение над этим видео (фон ответил сторожу `ask_viewer`). */
  offerStood(tabId: number, ref: VideoRef): void;
  /** Выбор человека. Не отклоняется: сбой — `no_offer` (видео тогда играет в браузере). */
  choose(tabId: number, frameId: number, message: ViewerOfferChoiceMessage): Promise<ViewerOfferAnswer>;
  /** Вкладка закрыта (`tabs.onRemoved`): её предложение забыть. */
  forgetTab(tabId: number): void;
  /** Предложение вкладки (для тестов и диагностики). */
  offerOf(tabId: number): VideoRef | undefined;
}

/** Та же ли это ссылка на видео (фрейм, документ, элемент). */
function sameVideo(left: VideoRef, right: VideoRef): boolean {
  return left.frame_id === right.frame_id && left.document === right.document && left.element === right.element;
}

export function createViewerOffers(dependencies: ViewerOffersDependencies): ViewerOffers {
  /** Висящие предложения: вкладка → видео. */
  const offers = new Map<number, VideoRef>();

  /** Ключ, на который пишется правило: там, где своё правило уже записано, иначе точный сайт вкладки. */
  function ruleKeyOf(siteKey: string, settings: AutoHandoffSettings): string {
    return siteRuleEntryFor(siteKey, settings.site_rules)?.siteKey ?? siteKey;
  }

  /** Записать правило сайта вкладки. Сбой — в журнал: действие человека от этого не отменяется. */
  async function saveRule(siteKey: string | undefined, settings: AutoHandoffSettings, rule: SiteRule): Promise<void> {
    if (siteKey === undefined) {
      dependencies.note("the viewer offer rule has no site to be written for", rule);
      return;
    }
    const edit = withSiteRuleChoice(settings, ruleKeyOf(siteKey, settings), rule);
    if (edit.kind === "rejected") {
      dependencies.note("the viewer offer rule was rejected", edit.reason);
      return;
    }
    if (edit.kind === "unchanged") return;
    try {
      await dependencies.writeSettings(edit.settings);
    } catch (error) {
      dependencies.note("could not save the viewer offer rule", error);
    }
  }

  async function choose(tabId: number, frameId: number, message: ViewerOfferChoiceMessage): Promise<ViewerOfferAnswer> {
    const ref: VideoRef = { frame_id: frameId, document: message.document, element: message.element };
    const remembered = offers.get(tabId);
    const settings = await dependencies.readSettings();
    const url = (await dependencies.describeTab(tabId))?.url ?? "";
    // Предложение узнаём по памяти, а после сна фона — по тому, что сайт вкладки и сейчас `ask` (см. шапку).
    const known = remembered !== undefined ? sameVideo(remembered, ref) : sitePolicyOf(url, settings).kind === "ask";
    if (!known) return "no_offer";
    offers.delete(tabId);
    const plan = planOfViewerChoice(message.choice);
    if (plan.rule !== undefined) await saveRule(parseWebAddress(url)?.siteKey, settings, plan.rule);
    if (plan.action === "play_here") return "play_here";
    dependencies.startHandoff(tabId, frameId, ref);
    return "hand_off";
  }

  return {
    offerStood(tabId, ref) {
      const previous = offers.get(tabId);
      offers.set(tabId, ref);
      if (previous !== undefined && !sameVideo(previous, ref)) dependencies.withdrawOffer(tabId, previous.frame_id, previous.element);
    },
    choose(tabId, frameId, message) {
      return choose(tabId, frameId, message).catch((error: unknown) => {
        dependencies.note("the viewer offer choice failed, the video plays here", error);
        return "no_offer";
      });
    },
    forgetTab(tabId) {
      offers.delete(tabId);
    },
    offerOf(tabId) {
      return offers.get(tabId);
    },
  };
}
