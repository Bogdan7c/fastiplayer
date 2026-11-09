// Адаптеры сайтов на стороне сеанса (09d, 14b): одна точка, через которую сеанс говорит со всеми способностями
// адаптеров — рекламой (`ad-skip-mirror.ts`) и вариантами страницы (`variant-mirror.ts`).
//
// Зачем общий модуль: у способностей одинаковый жизненный цикл (аренда наблюдения начинается, когда тень
// встала, и кончается со стороной страницы сеанса) и одни и те же нужды (вкладка, сокет, журнал). Сеанс
// (`handoff-session.ts`) упёрся в предел размера и знает только этот фасад; каждая способность владеет
// своим состоянием сама, фасад ничего не решает за них.

import type { PageAdState, PageSessionMessage, ProtocolVersion, VariantKind } from "../../protocol/index.ts";
import type { VariantWatchReport } from "../../shared/variant-watch-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createSessionAdMirror, type AdMirrorBrowser } from "./ad-skip-mirror.ts";
import { createSessionVariantMirror, type VariantMirrorBrowser } from "./variant-mirror.ts";

export interface SessionSiteAdaptersDependencies {
  /** Вкладка страницы сеанса сейчас (после возврата страницы, 11b2, — другая). */
  readonly currentTabId: () => number;
  /** Фрейм видео сеанса: его меню вариантов главнее. */
  readonly sessionFrameId: () => number | undefined;
  readonly scheduler: Scheduler;
  /** Что способностям нужно от браузера (в сеансе — его зависимости целиком). */
  readonly browsers: { readonly adMirrorBrowser: AdMirrorBrowser; readonly variantMirrorBrowser: VariantMirrorBrowser };
  /** Признак сеанса «идёт ролик, принятый за рекламу». */
  readonly sessionSaysAdOnScreen: () => boolean;
  readonly agreedProtocol: () => ProtocolVersion;
  /** Отправить сообщение плееру; сокета ещё или уже нет — сообщение никому не нужно. */
  readonly send: (message: PageSessionMessage) => void;
  readonly note: (message: string) => void;
}

/** Команда плеера, которую исполняет адаптер сайта на странице. */
export type SiteAdapterCommand =
  | { readonly kind: "skip_ad" }
  | { readonly kind: "select_variant"; readonly variantKind: VariantKind; readonly variantId: string }
  | { readonly kind: "next_episode" };

export interface SessionSiteAdapters {
  /** Тень встала на ролик (первый или после переезда). */
  videoStarted(): void;
  /** Донесения фреймов (форма проверена). */
  adWatchReport(frameId: number, ad: PageAdState): void;
  variantWatchReport(frameId: number, report: VariantWatchReport): void;
  /** Команда плеера странице. */
  command(command: SiteAdapterCommand): void;
  /** Сторона страницы сеанса остановлена: аренды не продлеваются, донесения не принимаются. */
  stop(): void;
}

export function createSessionSiteAdapters(dependencies: SessionSiteAdaptersDependencies): SessionSiteAdapters {
  const adMirror = createSessionAdMirror({
    currentTabId: dependencies.currentTabId,
    scheduler: dependencies.scheduler,
    browser: dependencies.browsers.adMirrorBrowser,
    sessionSaysAdOnScreen: dependencies.sessionSaysAdOnScreen,
    sendAdState: (ad) => dependencies.send({ type: "ad_state", ad }),
    note: dependencies.note,
  });
  const variantMirror = createSessionVariantMirror({
    currentTabId: dependencies.currentTabId,
    sessionFrameId: dependencies.sessionFrameId,
    scheduler: dependencies.scheduler,
    browser: dependencies.browsers.variantMirrorBrowser,
    agreedProtocol: dependencies.agreedProtocol,
    send: dependencies.send,
    note: dependencies.note,
  });

  return {
    videoStarted() {
      adMirror.videoStarted();
      variantMirror.videoStarted();
    },
    adWatchReport: (frameId, ad) => adMirror.acceptFrameReport(frameId, ad),
    variantWatchReport: (frameId, report) => variantMirror.acceptFrameReport(frameId, report),
    command(command) {
      switch (command.kind) {
        case "skip_ad":
          return adMirror.skipAd();
        case "select_variant":
          // Субтитры приходят дорожками 13b, меню субтитров мы не шлём: такой выбор — не наш.
          if (command.variantKind === "subtitles") return dependencies.note("select_variant ignored: subtitles are page tracks");
          return variantMirror.selectVariant(command.variantKind, command.variantId);
        case "next_episode":
          return variantMirror.nextEpisode();
      }
    },
    stop() {
      adMirror.stop();
      variantMirror.stop();
    },
  };
}
