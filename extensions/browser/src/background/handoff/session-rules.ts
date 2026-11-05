// Сессионные правила `declarativeNetRequest` фона. Единственный владелец этого набора у браузера:
// номера правил, уборка после прошлой жизни service worker, постоянные правила и правила на один запрос.
// Что именно правила делают, решают их модули: file-referrer-rule.ts и local-address-block-rules.ts.
//
// Как устроено:
// - сессионные правила переживают перезапуск service worker, а счётчик номеров нет. Поэтому на старте
//   фона одним атомарным обновлением снимается всё старое и ставятся постоянные правила; правила на
//   запрос ждут этого шага, и окна без постоянных правил нет;
// - правило на запрос живёт ровно один запрос: ставится перед `fetch`, снимается, когда запрос ответил,
//   упал или оборван. Заголовки к этому моменту уже ушли, чтение тела правила не требует;
// - установка ограничена: зависший API правил не держит загрузчик, а обрыв запроса (перемотка, конец
//   сеанса) не ждёт ответа API. Правило, поставленное уже после отказа от него, снимается сразу;
// - сбой API пишется в журнал один раз. Что делать с запросом без правила, решает вызывающий.

import type { Scheduler } from "../host-link/native-port.ts";

/** Запросы не из вкладки, то есть запросы самого фона (`tabs.TAB_ID_NONE`). */
export const NO_TAB_ID = -1;

/**
 * Приоритеты правил. Порядок важен: Chrome применяет заголовки только у правил с приоритетом СТРОГО
 * выше подходящего `allow`, поэтому Referer на разрешённом локальном запросе без верхней ступени пропал бы.
 */
export const SESSION_RULE_PRIORITY = { block: 1, allow: 2, header: 3 } as const;

/** Постоянным правилам отведены номера ниже этого; правила на запрос нумеруются отсюда вверх. */
export const FIRST_REQUEST_RULE_ID = 1_000;

/**
 * Сколько ждём установки правила, мс. API отвечает за миллисекунды; 2 с — запас на занятый браузер, и
 * это меньше паузы, которую плеер заметил бы как обрыв чтения.
 */
export const RULE_INSTALL_TIMEOUT_MS = 2_000;

/** Действие правила в той мере, в какой оно нужно фону (форма `declarativeNetRequest.RuleAction`). */
export type SessionRuleAction =
  | { readonly type: "block" }
  | { readonly type: "allow" }
  | {
      readonly type: "modifyHeaders";
      readonly requestHeaders: readonly { readonly header: string; readonly operation: "set"; readonly value: string }[];
    };

/** Условие правила (форма `declarativeNetRequest.RuleCondition`). */
export interface SessionRuleCondition {
  readonly tabIds: readonly number[];
  readonly resourceTypes: readonly string[];
  readonly urlFilter?: string;
  readonly regexFilter?: string;
  readonly isUrlFilterCaseSensitive?: boolean;
  readonly initiatorDomains?: readonly string[];
}

/** Правило без номера: номер раздаёт владелец набора. */
export interface SessionRuleDraft {
  readonly priority: number;
  readonly action: SessionRuleAction;
  readonly condition: SessionRuleCondition;
}

export interface SessionRule extends SessionRuleDraft {
  readonly id: number;
}

/** Шов к браузеру: в продукте — `declarativeNetRequest`, в тестах — фейк. */
export interface SessionRulesApi {
  updateSessionRules(options: { readonly removeRuleIds?: readonly number[]; readonly addRules?: readonly SessionRule[] }): Promise<void>;
  getSessionRules(): Promise<readonly { readonly id: number }[]>;
}

/** Правила на один запрос. */
export interface RequestRules {
  /**
   * Дождаться старта набора, то есть постоянных правил. Нужен запросу без своего правила: он тоже обязан
   * идти после заслона. Ожидание ограничено теми же сроком и обрывом запроса, что и установка.
   */
  whenPrepared(signal: AbortSignal): Promise<void>;
  /** Поставить правило; `undefined` — не вышло (сбой, срок вышел или запрос оборван). */
  install(draft: SessionRuleDraft, signal: AbortSignal): Promise<number | undefined>;
  /** Снять правило; ответа не ждём: запросу это уже не нужно, а зависший API не должен держать закачку. */
  remove(id: number): void;
}

export interface SessionRulesParameters {
  readonly api: SessionRulesApi;
  readonly scheduler: Pick<Scheduler, "setTimeout" | "clearTimeout">;
  /** Правила на всю жизнь фона; номера ниже `FIRST_REQUEST_RULE_ID`. */
  readonly permanentRules: readonly SessionRule[];
  /** Сбой API правил; вызывается один раз за жизнь фона. */
  readonly reportFailureOnce: (error: unknown) => void;
}

/** Чем кончилась установка, ограниченная сроком и обрывом запроса. */
type InstallSettlement = { readonly kind: "installed" } | { readonly kind: "failed"; readonly error: unknown } | { readonly kind: "timed_out" } | { readonly kind: "aborted" };

export function createSessionRules(parameters: SessionRulesParameters): RequestRules {
  const { api, scheduler, permanentRules } = parameters;
  let nextRuleId = FIRST_REQUEST_RULE_ID;
  let failureReported = false;

  function reportFailure(error: unknown): void {
    if (failureReported) return;
    failureReported = true;
    parameters.reportFailureOnce(error);
  }

  /** Номера правил прошлой жизни фона; сбой не страшен: добавление всё равно заменяет по номеру. */
  async function staleRuleIds(): Promise<readonly number[]> {
    try {
      return (await api.getSessionRules()).map((rule) => rule.id);
    } catch (error) {
      reportFailure(error);
      return [];
    }
  }

  /** Старт фона: снять всё старое и поставить постоянные правила одним обновлением. */
  async function prepare(): Promise<void> {
    const removeRuleIds = [...new Set([...(await staleRuleIds()), ...permanentRules.map((rule) => rule.id)])];
    if (removeRuleIds.length === 0) return;
    try {
      await api.updateSessionRules({ removeRuleIds, addRules: permanentRules });
    } catch (error) {
      reportFailure(error);
    }
  }

  const prepared = prepare();

  /** Дождаться установки, срока или обрыва запроса: что наступит раньше. */
  function settle(added: Promise<void>, signal: AbortSignal): Promise<InstallSettlement> {
    return new Promise((resolve) => {
      const finish = (settlement: InstallSettlement): void => {
        scheduler.clearTimeout(timer);
        signal.removeEventListener("abort", onAbort);
        resolve(settlement);
      };
      const onAbort = (): void => finish({ kind: "aborted" });
      const timer = scheduler.setTimeout(() => finish({ kind: "timed_out" }), RULE_INSTALL_TIMEOUT_MS);
      signal.addEventListener("abort", onAbort, { once: true });
      added.then(
        () => finish({ kind: "installed" }),
        (error: unknown) => finish({ kind: "failed", error }),
      );
    });
  }

  function remove(id: number): void {
    api.updateSessionRules({ removeRuleIds: [id] }).catch(reportFailure);
  }

  return {
    async whenPrepared(signal) {
      const settlement = await settle(prepared, signal);
      if (settlement.kind === "timed_out") reportFailure(new Error("declarativeNetRequest did not answer in time"));
    },
    async install(draft, signal) {
      if (signal.aborted) return undefined;
      const id = nextRuleId;
      nextRuleId += 1;
      const added = prepared.then(() => api.updateSessionRules({ removeRuleIds: [id], addRules: [{ ...draft, id }] }));
      const settlement = await settle(added, signal);
      switch (settlement.kind) {
        case "installed":
          return id;
        case "failed":
          reportFailure(settlement.error);
          return undefined;
        case "timed_out":
          reportFailure(new Error("declarativeNetRequest did not answer in time"));
          break;
        case "aborted":
          break;
      }
      // От правила отказались, но API ещё может его поставить: чужому запросу оно не нужно.
      added.then(() => remove(id), reportFailure);
      return undefined;
    },
    remove,
  };
}

/**
 * Выполнить запрос под правилом на один запрос. Правило не встало — запрос всё равно уходит: что это
 * значит, зависит от правила, и об этом говорит вызывающий. Оборванный запрос в сеть не идёт.
 */
export async function underRequestRule<Result>(rules: RequestRules, draft: SessionRuleDraft, signal: AbortSignal, send: () => Promise<Result>): Promise<Result> {
  const ruleId = await rules.install(draft, signal);
  signal.throwIfAborted();
  try {
    return await send();
  } finally {
    if (ruleId !== undefined) rules.remove(ruleId);
  }
}
