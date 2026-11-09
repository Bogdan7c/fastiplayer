// Первый вопрос плееру о «дешёвых» кодеках (сессия 13a, решение 266).
//
// Метка кодеков приходит только в `hello_accepted`, а `hello` раньше уходил лишь по делу — на первой
// передаче видео. Поэтому самое первое видео после установки шло без политики: сайт уже выбрал
// AV1/VP9, когда экстеншен впервые узнавал, что плееру дёшев только H.264.
//
// Решение владельца: пока метка неизвестна, при установке и при старте браузера фон ОДИН раз просит
// связь с посредником. Посредник будит процесс браузерного режима, тот отвечает `hello_accepted` с
// меткой и без сеанса сам выходит через 30 с; обычный плеер `hello` не трогает (пауза — только при
// выдаче сеанса). Как только метка известна (даже пустая — «ограничений нет»), вопрос больше не
// задаётся: дальше метка обновляется каждой передачей, как раньше.
//
// Модуль не владеет ни портом, ни кэшем: связь — у host-link (`acquireDemand`), кэш — у сервиса политики.
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";

/**
 * Сколько держать нужду в ожидании ответа. Дверь D-Bus даёт активации процесса до 15 с, `hello` —
 * свои 8 с; сверху запас на один повтор связи. Не ответил — отпускаем: следующий старт браузера
 * или первая передача спросят снова.
 */
export const FIRST_LABEL_WAIT_MS = 30_000;

export interface FirstLabelRequestDeps {
  /** Известна ли уже метка плеера (сервис политики). */
  readonly knowsPlayerLabel: () => Promise<boolean>;
  /** Взять нужду в связи с посредником (host-link); вернувшаяся функция её отпускает. */
  readonly acquireDemand: (reason: string) => () => void;
  readonly scheduler: Scheduler;
}

export interface FirstLabelRequest {
  /** Установка экстеншена или старт браузера: спросить плеер, если метка ещё неизвестна. Не бросает. */
  askIfUnknown(): Promise<void>;
  /** Плеер ответил `hello_accepted` (метка пришла): отпустить нужду, если она наша. */
  playerAnswered(): void;
}

/** Состояние вопроса: не задан, ждём ответа (держим нужду). */
type Phase = { readonly kind: "idle" } | { readonly kind: "waiting"; readonly release: () => void };

export function createFirstLabelRequest(deps: FirstLabelRequestDeps): FirstLabelRequest {
  const waitTimer = createTimerSlot(deps.scheduler);
  let phase: Phase = { kind: "idle" };
  // Пока идёт чтение кэша, второй вызов не должен задать второй вопрос.
  let checking = false;

  function finish(): void {
    if (phase.kind !== "waiting") return;
    waitTimer.cancel();
    phase.release();
    phase = { kind: "idle" };
  }

  return {
    async askIfUnknown() {
      if (phase.kind === "waiting" || checking) return;
      checking = true;
      let known: boolean;
      try {
        known = await deps.knowsPlayerLabel();
      } finally {
        checking = false;
      }
      // Метка уже есть — плеер не будим. (Второй вопрос за время чтения кэша невозможен: его
      // не пускает флаг `checking`, а ждать начинает только эта ветка.)
      if (known) return;
      phase = { kind: "waiting", release: deps.acquireDemand("codec-label") };
      waitTimer.start(FIRST_LABEL_WAIT_MS, finish);
    },
    playerAnswered: finish,
  };
}
