// Бюджет событий страницы (решение 200): вёдра по видам, разовый запас на повтор журнала.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createPageEventBudget, JOURNAL_REPLAY_EVENT_CREDIT, PAGE_EVENT_LIMITS_PER_SECOND, type PageEventBudget } from "../../src/content/page-event-budget.ts";
import type { PageEventBudgetKind } from "../../src/shared/handoff-contract.ts";
import { MAX_JOURNAL_ENTRIES } from "../../src/shared/page-message-validation.ts";

const SMALL_LIMITS: Readonly<Record<PageEventBudgetKind, number>> = {
  mse: 4,
  page_seek: 2,
  page_playback: 2,
  player_seek_applied: 2,
  file_source: 1, hls_source: 1,
  shadow_alive: 2,
  next_episode_wait: 1,
  subtitle: 2,
  malformed: 3,
};

function setupBudget(replayEventCredit = 0) {
  const clock = { nowMs: 0 };
  const budget = createPageEventBudget({ now: () => clock.nowMs, limitsPerSecond: SMALL_LIMITS, replayEventCredit });
  return { clock, budget };
}

function admitted(budget: PageEventBudget, kind: PageEventBudgetKind, count: number): number {
  let passed = 0;
  for (let index = 0; index < count; index += 1) if (budget.admit(kind) === "admitted") passed += 1;
  return passed;
}

describe("бюджет событий передачи", () => {
  it("за секунду проходит ровно лимит вида, следующее событие — превышение", () => {
    const { budget } = setupBudget();
    assert.equal(admitted(budget, "page_seek", 2), 2);
    assert.equal(budget.admit("page_seek"), "exceeded");
  });

  it("окно скользит: через полсекунды есть половина лимита, через секунду — весь, не больше", () => {
    const { budget, clock } = setupBudget();
    admitted(budget, "mse", 4);
    clock.nowMs = 500;
    assert.equal(admitted(budget, "mse", 3), 2);
    clock.nowMs = 60_000;
    assert.equal(admitted(budget, "mse", 10), 4, "простой не копит больше секундного запаса");
  });

  it("виды считаются порознь: флуд одного вида не тратит другой", () => {
    const { budget } = setupBudget();
    admitted(budget, "mse", 100);
    assert.equal(budget.admit("page_seek"), "admitted");
    assert.equal(budget.admit("malformed"), "admitted");
  });

  it("запас повтора: все записи проходят мгновенно, но только один раз и только `mse`", () => {
    const { budget, clock } = setupBudget(10);
    assert.equal(admitted(budget, "mse", 14), 14, "лимит 4 плюс запас 10");
    assert.equal(budget.admit("mse"), "exceeded");
    clock.nowMs = 1_000;
    assert.equal(admitted(budget, "mse", 15), 4, "запас не возвращается: дальше только обычный лимит");
  });

  it("запас не достаётся другим видам", () => {
    const { budget } = setupBudget(100);
    assert.equal(admitted(budget, "malformed", 50), 3);
    assert.equal(admitted(budget, "page_seek", 50), 2);
  });

  it("боевые числа: запас покрывает журнал у потолка, лимиты в разы выше замеров живых сайтов", () => {
    assert.ok(JOURNAL_REPLAY_EVENT_CREDIT >= MAX_JOURNAL_ENTRIES);
    // Пики замера (шапка page-event-budget.ts): куски 246/с — опора для `mse`, перемотки 2/с, play/pause 5/с.
    assert.ok(PAGE_EVENT_LIMITS_PER_SECOND.mse >= 246 * 8);
    assert.ok(PAGE_EVENT_LIMITS_PER_SECOND.page_seek >= 2 * 8);
    assert.ok(PAGE_EVENT_LIMITS_PER_SECOND.page_playback >= 5 * 8);
    // Тысячи мусорных сообщений в секунду лимит обязан ловить.
    assert.ok(PAGE_EVENT_LIMITS_PER_SECOND.malformed < 1_000);
  });
});
