// Адаптеры сайтов на стороне сеанса (09d, 14b): одна точка, через которую сеанс говорит со всеми способностями
// адаптеров — рекламой (`ad-skip-mirror.ts`), вариантами страницы (`variant-mirror.ts`) и памятью выбора
// (`choice-memory/session-choice-memory.ts`, 14b2): память слушает зеркало вариантов и выбирает через него.
//
// Зачем общий модуль: у способностей одинаковый жизненный цикл (аренда наблюдения начинается, когда тень
// встала, и кончается со стороной страницы сеанса) и одни и те же нужды (вкладка, сокет, журнал). Сеанс
// (`handoff-session.ts`) упёрся в предел размера и знает только этот фасад; каждая способность владеет
// своим состоянием сама, фасад ничего не решает за них.

import {
  pageSessionIntroducedIn,
  versionIncludes,
  type PageAdState,
  type PageSessionMessage,
  type ClockState,
  type ProtocolVersion,
  type SubtitleChoice,
  type VariantKind,
} from "../../protocol/index.ts";
import type { VariantWatchReport } from "../../shared/variant-watch-contract.ts";
import type { ChoiceMemoryStore } from "../choice-memory/choice-memory-store.ts";
import type { OfferedPageTrack } from "../choice-memory/page-track-catalog.ts";
import { pageSiteKeyOf } from "../choice-memory/series-fingerprint.ts";
import { createSessionChoiceMemory, type ChoiceMemoryWriting } from "../choice-memory/session-choice-memory.ts";
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
  /** Память выбора (14b2): хранилище фона, адрес страницы сейчас и можно ли в этом сеансе писать. */
  readonly choiceMemory: {
    readonly store: ChoiceMemoryStore;
    readonly pageUrl: () => string | undefined;
    readonly writing: ChoiceMemoryWriting;
  };
}

/**
 * Можно ли писать память для вкладки (решение 285): обычное окно — да, приватное — нет. Браузер не сказал,
 * приватное ли окно, — тоже нет: молча записать приватный выбор хуже, чем не запомнить обычный.
 */
export function choiceMemoryWritingOf(incognito: boolean | undefined): ChoiceMemoryWriting {
  if (incognito === undefined) return "privacy_unknown";
  return incognito ? "private_window" : "allowed";
}

/** Команда плеера, которую исполняет адаптер сайта на странице. */
export type SiteAdapterCommand =
  | { readonly kind: "skip_ad" }
  | { readonly kind: "select_variant"; readonly variantKind: VariantKind; readonly variantId: string }
  | { readonly kind: "next_episode" }
  /** Зритель выбрал субтитры в плеере (протокол 1.7): это не команда странице, а выбор для памяти. */
  | { readonly kind: "subtitle_choice"; readonly choice: SubtitleChoice };

export interface SessionSiteAdapters {
  /** Тень встала на ролик (первый или после переезда). */
  videoStarted(): void;
  /** Донесения фреймов (форма проверена). */
  adWatchReport(frameId: number, ad: PageAdState): void;
  variantWatchReport(frameId: number, report: VariantWatchReport): void;
  /** Команда плеера странице. */
  command(command: SiteAdapterCommand): void;
  /** Часы плеера (`clock`): память выбора решает, у начала ли серия. */
  playerClock(positionUs: number, state: ClockState): void;
  /** Дорожку субтитров страницы предложили плееру: память узнает её язык и подпись. */
  subtitleTrackOffered(track: OfferedPageTrack): void;
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
  const choiceMemory = createSessionChoiceMemory({
    store: dependencies.choiceMemory.store,
    pageSiteKey: () => pageSiteKeyOf(dependencies.choiceMemory.pageUrl()),
    writing: dependencies.choiceMemory.writing,
    adOnScreen: dependencies.sessionSaysAdOnScreen,
    // Зеркало создаётся ниже; память зовёт его только из своих событий, а их шлёт уже созданное зеркало.
    selectOnPage: (kind, variantId) => variantMirror.selectRemembered(kind, variantId),
    sendSubtitlePreference(choice) {
      // Плеер старше 1.7 не знает подсказки и рвал бы сеанс: молчим.
      if (!versionIncludes(dependencies.agreedProtocol(), pageSessionIntroducedIn("subtitle_preference"))) return "player_too_old";
      dependencies.send({ type: "subtitle_preference", choice });
      return "sent";
    },
    now: () => dependencies.scheduler.now(),
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
    observer: choiceMemory,
  });

  return {
    videoStarted() {
      choiceMemory.resume();
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
          // Все три вида (в том числе субтитры страницы, 15a2) идут тем же путём: во фрейм меню этого вида.
          return variantMirror.selectVariant(command.variantKind, command.variantId);
        case "next_episode":
          return variantMirror.nextEpisode();
        case "subtitle_choice":
          return choiceMemory.viewerSubtitleChoice(command.choice);
      }
    },
    playerClock: (positionUs, state) => choiceMemory.playerClock(positionUs, state),
    subtitleTrackOffered: (track) => choiceMemory.subtitleTrackOffered(track),
    stop() {
      adMirror.stop();
      variantMirror.stop();
      choiceMemory.stop();
    },
  };
}
