// Разовая проба «есть ли на вкладке реклама на экране» (сессия 12a) для короткого ролика, который человек
// запустил сам: реклама перед фильмом (в нашем плеере, переезд сеанса 09c2 перенесёт его на фильм) или
// обычный короткий ролик (отпускаем и даём кредит клика).
//
// У вкладки без сеанса донесений о рекламе нет: аренду наблюдения (`arm_ad_watch`) держит только сеанс.
// Проба берёт аренду на короткий срок всем фреймам; фрейм читает органы рекламы сразу при аренде и докладывает.
// Первое донесение «реклама показана» -> `ad_on_screen`. Тишина до конца окна -> `no_ad`.
//
// Донесения чужих вкладок пробу не трогают. Две пробы одной вкладки делят одно окно и один ответ.

import type { PageAdState } from "../../protocol/index.ts";
import type { Scheduler, TimerHandle } from "../host-link/native-port.ts";
import type { AdMirrorBrowser } from "../handoff/ad-skip-mirror.ts";

/**
 * Окно ожидания донесений, мс. Фрейм читает органы рекламы сразу при получении аренды (пара
 * миллисекунд плюс доставка сообщения), поэтому 600 мс хватает с запасом, а человек ещё не замечает паузу
 * (видео всё это время задержано под тенью, звука нет).
 */
export const AD_PROBE_WINDOW_MS = 600;

/** Аренда наблюдения на время пробы, мс: чуть длиннее окна, чтобы фрейм не снял наблюдение раньше ответа. */
export const AD_PROBE_LEASE_MS = 1_500;

/** Итог пробы (без `not_probed`: проба уже была). */
export type AdProbeResult = "ad_on_screen" | "no_ad";

export interface AdProbeDependencies {
  readonly scheduler: Scheduler;
  /** Аренда наблюдения всем фреймам вкладки (тот же шов, что у сеанса). */
  readonly armAdWatch: AdMirrorBrowser["armAdWatch"];
}

export interface AdProbe {
  /** Проверить вкладку. Не отклоняется. */
  probe(tabId: number): Promise<AdProbeResult>;
  /** Донесение фрейма о рекламе (из того же слушателя, что кормит сеанс). Вкладка без пробы — игнорируется. */
  acceptReport(tabId: number, frameId: number, ad: PageAdState): void;
}

interface PendingProbe {
  readonly result: Promise<AdProbeResult>;
  readonly settle: (result: AdProbeResult) => void;
}

export function createAdProbe(dependencies: AdProbeDependencies): AdProbe {
  const pending = new Map<number, PendingProbe>();

  function startProbe(tabId: number): PendingProbe {
    let timer: TimerHandle | undefined;
    let settle: (result: AdProbeResult) => void = () => {};
    const result = new Promise<AdProbeResult>((resolve) => {
      settle = (value) => {
        if (timer !== undefined) dependencies.scheduler.clearTimeout(timer);
        pending.delete(tabId);
        resolve(value);
      };
    });
    timer = dependencies.scheduler.setTimeout(() => settle("no_ad"), AD_PROBE_WINDOW_MS);
    const probe: PendingProbe = { result, settle };
    pending.set(tabId, probe);
    dependencies.armAdWatch(tabId, AD_PROBE_LEASE_MS);
    return probe;
  }

  return {
    probe: (tabId) => (pending.get(tabId) ?? startProbe(tabId)).result,
    acceptReport(tabId, _frameId, ad) {
      if (ad.kind === "showing") pending.get(tabId)?.settle("ad_on_screen");
    },
  };
}
