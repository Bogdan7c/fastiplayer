// Исполнитель руки-функции адаптеров в MAIN-мире (сессия 14b, решение 282).
//
// По вызову изолированного мира находит действие в белом списке сайта (`registry/page-world.ts`), зовёт
// его в окне страницы и отвечает копией результата. Действие вне списка не исполняется никогда: имя
// сайта и действия — ключи списка, а не код.
//
// Страница — враг, и её функции могут что угодно: бросить, вернуть прокси, вечное обещание. Поэтому:
// - всё под try/catch, исключение в страницу не уходит;
// - результат копируется в `HandValue` (`copyHandValue`): геттеры и прототипы страницы дальше не идут;
// - вечное обещание не держит изолированный мир: у него свой срок ответа. Здесь ответа просто не будет.

import { playerHandActionsOf } from "../adapters/registry/page-world.ts";
import { PAGE_FUNCTION_MISSING, type PlayerHandActions } from "../adapters/capabilities/player-hand/contract.ts";
import { copyHandValue, type AdapterHandCall, type AdapterHandOutcome, type AdapterHandReply } from "../shared/adapter-hand-contract.ts";

export interface AdapterHandExecutorDependencies {
  /** Окно страницы: в нём действия ищут функции плеера сайта. */
  readonly page: object;
  /** Белый список сайта по имени; в продукте — реестр мира страницы, в тестах — свой. */
  readonly actionsOf?: (siteId: string) => PlayerHandActions | undefined;
  /** Ответ изолированному миру по личному порту; `undefined` — порта ещё нет (ответ теряется). */
  readonly send: (reply: AdapterHandReply) => void;
}

export interface AdapterHandExecutor {
  /** Исполнить вызов и ответить. Не бросает. */
  handle(call: AdapterHandCall): Promise<void>;
}

/** Исход одного действия: `PAGE_FUNCTION_MISSING` — функции у сайта нет; остальное — копия результата. */
async function runAction(action: PlayerHandActions[string], page: object, argument: string | null): Promise<AdapterHandOutcome> {
  try {
    // `await` разворачивает и обещание сайта; бросок внутри него — тот же `failed`.
    const result: unknown = await action(page, argument);
    if (result === PAGE_FUNCTION_MISSING) return { kind: "no_such_action" };
    // `undefined` (функция ничего не вернула) — это «сделано без значения».
    const value = copyHandValue(result === undefined ? null : result);
    return value === undefined ? { kind: "failed" } : { kind: "done", value };
  } catch {
    return { kind: "failed" };
  }
}

export function createAdapterHandExecutor(dependencies: AdapterHandExecutorDependencies): AdapterHandExecutor {
  const actionsOf = dependencies.actionsOf ?? playerHandActionsOf;
  return {
    async handle(call: AdapterHandCall): Promise<void> {
      const actions = actionsOf(call.site);
      // `Object.hasOwn`: имя вроде `constructor` или `toString` не должно найти функцию прототипа.
      const action = actions !== undefined && Object.hasOwn(actions, call.action) ? actions[call.action] : undefined;
      const outcome: AdapterHandOutcome = action === undefined ? { kind: "no_such_action" } : await runAction(action, dependencies.page, call.argument);
      try {
        dependencies.send({ kind: "adapter_hand_reply", request_id: call.request_id, outcome });
      } catch {
        // Порт закрыт: ответа никто не ждёт, у изолированного мира свой срок.
      }
    },
  };
}
