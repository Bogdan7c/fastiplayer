// Дублёр кнопки пропуска рекламы на стороне сеанса (сессия 09d): склеивает сводку `ad-on-screen.ts`,
// аренду наблюдения во фреймах и команду плеера `skip_ad`.
//
// Что принадлежит сеансу, а что нет: фон не читает страницу сам. Читают фреймы (изолированный мир),
// а фон держит аренду, собирает донесения и отвечает на команды плеера. Пока передача не началась
// (`videoStarted` не вызывали) и после конца сеанса (`stop`), фон и страница молчат (решение 198).
//
// Правила команд:
// - `skip_ad` идёт во фрейм, который сказал `available`, и больше никуда: чужие кнопки не трогаем.
//   Кнопки уже нет (команда устарела) — это не ошибка, а отметка в журнале;
// - кнопка со словом `on_page` — «нажимает человек на странице»: команды на неё нет (решение 208).

import type { PageAdState } from "../../protocol/index.ts";
import type { PressSkipOutcome } from "../../shared/ad-watch-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";
import { createAdOnScreenSummary, type AdOnScreenSummary } from "./ad-on-screen.ts";

/** Срок одной аренды наблюдения: три продления подряд могут потеряться. */
export const AD_WATCH_LEASE_MS = 3_000;

/** Как часто фон продлевает аренду; с той же частотой пересчитывается сводка (устаревшие донесения). */
export const AD_WATCH_RENEW_MS = 1_000;

/**
 * Не чаще раза в столько миллисекунд `ad_state` уходит плееру (весь сеанс, все фреймы вместе).
 * Лимит донесений фрейма не спасает: фреймов может быть много. Отсчёт сайта меняется раз в секунду,
 * смена рекламы редка; страница, мигающая числами, не должна заливать журнал и окно плеера.
 */
export const AD_STATE_MIN_GAP_MS = 250;

/** Что нужно сеансу от браузера. */
export interface AdMirrorBrowser {
  /** Взять наблюдение в аренду у всех фреймов вкладки. Сбой доставки не важен: продление повторит. */
  armAdWatch(tabId: number, leaseMs: number): void;
  /** Попросить один фрейм нажать кнопку пропуска; отказ доставки — исключение. */
  pressSkipInFrame(tabId: number, frameId: number): Promise<PressSkipOutcome>;
}

export interface SessionAdMirrorDependencies {
  /** Вкладка страницы сеанса сейчас: после возврата страницы (11b2) это уже другая вкладка. */
  readonly currentTabId: () => number;
  readonly scheduler: Scheduler;
  readonly browser: AdMirrorBrowser;
  /** Признак сеанса «идёт ролик, принятый за рекламу» (`relocation.adOnScreen`). */
  readonly sessionSaysAdOnScreen: () => boolean;
  /** Сводка изменилась: отправить плееру `ad_state`. */
  readonly sendAdState: (ad: PageAdState) => void;
  /** Отметка в журнал разработчика (пользовательского журнала в продукте нет). */
  readonly note: (message: string) => void;
}

export interface SessionAdMirror {
  /** Тень встала на ролик (первый или после переезда): начать аренду, если не начата, и пересчитать сводку. */
  videoStarted(): void;
  /** Донесение фрейма (форма уже проверена). Вне живого сеанса игнорируется. */
  acceptFrameReport(frameId: number, ad: PageAdState): void;
  /** Плеер просит нажать кнопку пропуска. */
  skipAd(): void;
  /** Сеанс кончился: аренда не продлевается, донесения не принимаются. */
  stop(): void;
}

export function createSessionAdMirror(dependencies: SessionAdMirrorDependencies): SessionAdMirror {
  const { currentTabId, browser } = dependencies;
  const renewTimer = createTimerSlot(dependencies.scheduler);
  const announceTimer = createTimerSlot(dependencies.scheduler);
  let running = false;
  let lastAnnouncedAt = Number.NEGATIVE_INFINITY;
  // Слово, которое ждёт конца паузы. Последнее побеждает: промежуточные плееру не нужны.
  let waitingAd: PageAdState | undefined;

  function announceWaiting(): void {
    if (!running || waitingAd === undefined) return;
    const ad = waitingAd;
    waitingAd = undefined;
    lastAnnouncedAt = dependencies.scheduler.now();
    dependencies.sendAdState(ad);
  }

  /** Сводка изменилась: сразу, если пауза прошла, иначе одним словом в конце паузы. */
  function announce(ad: PageAdState): void {
    waitingAd = ad;
    const wait = lastAnnouncedAt + AD_STATE_MIN_GAP_MS - dependencies.scheduler.now();
    if (wait <= 0) announceWaiting();
    else if (!announceTimer.isActive()) announceTimer.start(wait, announceWaiting);
  }

  const summary: AdOnScreenSummary = createAdOnScreenSummary({
    now: () => dependencies.scheduler.now(),
    sessionSaysAdOnScreen: dependencies.sessionSaysAdOnScreen,
    onChange: announce,
  });

  /** Продлить аренду, пересчитать сводку (устаревшие донесения, признак сеанса), запланировать следующее продление. */
  function renew(): void {
    browser.armAdWatch(currentTabId(), AD_WATCH_LEASE_MS);
    summary.recompute();
    // Отправка могла синхронно закончить сеанс (сокет закрывается): тогда цепочка продлений обрывается.
    if (running) renewTimer.start(AD_WATCH_RENEW_MS, renew);
  }

  return {
    videoStarted() {
      if (!running) {
        running = true;
        renew();
        return;
      }
      summary.recompute();
    },
    acceptFrameReport(frameId, ad) {
      if (running) summary.reportFromFrame(frameId, ad);
    },
    skipAd() {
      if (!running) return;
      const frameId = summary.frameWithAvailableSkip();
      if (frameId === undefined) {
        dependencies.note("skip_ad ignored: no frame reports an available skip button");
        return;
      }
      browser.pressSkipInFrame(currentTabId(), frameId).then(
        (outcome) => {
          if (outcome !== "pressed") dependencies.note(`skip_ad not pressed: ${outcome}`);
        },
        (error: unknown) => dependencies.note(`skip_ad could not reach the frame: ${String(error)}`),
      );
    },
    stop() {
      running = false;
      waitingAd = undefined;
      renewTimer.cancel();
      announceTimer.cancel();
    },
  };
}
