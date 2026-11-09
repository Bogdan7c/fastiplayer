// Наблюдение за вариантами во фрейме (14b): аренда, слияние чтений, донесения при изменении и на продлении,
// лимит донесений, сбой адаптера и исполнение команд. Адаптер — подделка с управляемым описанием.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { AdElement } from "../../src/adapters/capabilities/ad-organs/contract.ts";
import type { PageVariantDescription, VariantActionOutcome, VariantAdapter } from "../../src/adapters/capabilities/variants/contract.ts";
import { createVariantWatch, VARIANT_READ_MIN_GAP_MS, VARIANT_REPORT_WINDOW_MS, VARIANT_REPORTS_PER_WINDOW } from "../../src/content/variant-watch.ts";
import type { VariantCommand, VariantWatchReport } from "../../src/shared/variant-watch-contract.ts";
import { createFakeClock, flushPromises } from "../background/support.ts";

const LEASE_MS = 3_000;
const ROOT: AdElement = { click: () => {} };

const DUB_A: PageVariantDescription = {
  seriesKey: "s",
  episodeKey: "1",
  audio: [
    { id: "dub-a", label: "Dub A", selected: true },
    { id: "dub-b", label: "Dub B", selected: false },
  ],
  quality: [],
  nextEpisode: false,
};
const DUB_B: PageVariantDescription = { ...DUB_A, audio: [{ ...DUB_A.audio[0]!, selected: false }, { ...DUB_A.audio[1]!, selected: true }] };

/** Подделка адаптера: что описывать, бросать ли, что ответить на команду. */
function fakeAdapter() {
  const state: { description: PageVariantDescription | null; matches: boolean; throws: boolean; outcome: VariantActionOutcome } = {
    description: DUB_A,
    matches: true,
    throws: false,
    outcome: "done",
  };
  const commands: VariantCommand[] = [];
  let describes = 0;
  const adapter: VariantAdapter = {
    matches: () => state.matches,
    observationRoot: () => ROOT,
    describe: async () => {
      describes += 1;
      if (state.throws) throw new Error("adapter broke");
      return state.description;
    },
    select: async (kind, id) => {
      commands.push({ kind: "select", variant_kind: kind, variant_id: id });
      if (state.throws) throw new Error("adapter broke");
      return state.outcome;
    },
    next: async () => {
      commands.push({ kind: "next" });
      return state.outcome;
    },
  };
  return { adapter, state, commands, describes: () => describes };
}

function harness() {
  const clock = createFakeClock();
  const fake = fakeAdapter();
  const reports: VariantWatchReport[] = [];
  const failures: string[] = [];
  let mutate: () => void = () => {};
  let observers = 0;
  const watch = createVariantWatch({
    adapters: [{ identity: { id: "fake", version: 2 }, adapter: fake.adapter }],
    place: { hostname: "127.0.0.1", contains: () => true },
    createWatcher: (onMutation) => {
      mutate = onMutation;
      return { observe: () => void (observers += 1), disconnect: () => void (observers -= 1) };
    },
    timers: clock.scheduler,
    now: clock.scheduler.now,
    sendReport: (report) => void reports.push(report),
    noteAdapterFailure: (identity, error) => void failures.push(`${identity.id}@${identity.version}: ${String(error)}`),
  });
  return { clock, fake, reports, failures, watch, mutate: () => mutate(), observers: () => observers };
}

const descriptions = (reports: readonly VariantWatchReport[]) => reports.map((report) => report.description);

describe("аренда", () => {
  it("до аренды ничего: ни чтений, ни наблюдателей, ни донесений (решение 198)", async () => {
    const world = harness();
    world.clock.advance(10_000);
    await flushPromises();
    assert.equal(world.fake.describes(), 0);
    assert.equal(world.observers(), 0);
    assert.deepEqual(world.reports, []);
  });

  it("аренда — сразу чтение и донесение с паспортом адаптера; срок вышел — наблюдатель снят, дальше тишина", async () => {
    const world = harness();
    world.watch.arm(LEASE_MS);
    await flushPromises();
    assert.deepEqual(world.reports, [{ kind: "variant_watch_report", adapter: "fake@2", description: DUB_A }]);
    assert.equal(world.observers(), 1);
    world.clock.advance(LEASE_MS);
    assert.equal(world.observers(), 0);
    assert.equal(world.watch.isWatching(), false);
    world.fake.state.description = DUB_B;
    world.mutate();
    world.clock.advance(1_000);
    await flushPromises();
    assert.equal(world.reports.length, 1);
  });

  it("каждое продление — пульс: то же описание уходит снова, чтобы фон не счёл меню ушедшим", async () => {
    const world = harness();
    for (let renewal = 0; renewal < 3; renewal += 1) {
      world.watch.arm(LEASE_MS);
      await flushPromises();
      world.clock.advance(1_000);
    }
    assert.deepEqual(descriptions(world.reports), [DUB_A, DUB_A, DUB_A]);
  });

  it("фрейм без адаптера молчит всегда: фон не будит ни одна чужая страница", async () => {
    const world = harness();
    world.fake.state.matches = false;
    world.watch.arm(LEASE_MS);
    await flushPromises();
    assert.deepEqual(world.reports, []);
    assert.equal(world.fake.describes(), 0);
  });
});

describe("перемены страницы", () => {
  it("мутации сливаются в одно чтение не чаще VARIANT_READ_MIN_GAP_MS; перемена уходит, повтор — нет", async () => {
    const world = harness();
    world.watch.arm(LEASE_MS);
    await flushPromises();
    const describesAfterArm = world.fake.describes();
    world.fake.state.description = DUB_B;
    for (let burst = 0; burst < 20; burst += 1) world.mutate();
    world.clock.advance(VARIANT_READ_MIN_GAP_MS);
    await flushPromises();
    assert.equal(world.fake.describes(), describesAfterArm + 1);
    world.mutate();
    world.clock.advance(VARIANT_READ_MIN_GAP_MS);
    await flushPromises();
    assert.deepEqual(descriptions(world.reports), [DUB_A, DUB_B]);
  });

  it("перемены чаще лимита — не больше VARIANT_REPORTS_PER_WINDOW донесений за окно, последнее состояние доходит потом", async () => {
    const world = harness();
    // Каждое продление читает сразу (без паузы слияния): пять разных описаний за 40 мс.
    const flips = [DUB_A, DUB_B, DUB_A, DUB_B, { ...DUB_A, nextEpisode: true }];
    for (const description of flips) {
      world.fake.state.description = description;
      world.watch.arm(60_000);
      await flushPromises();
      world.clock.advance(10);
    }
    assert.equal(world.reports.length, VARIANT_REPORTS_PER_WINDOW);
    world.clock.advance(VARIANT_REPORT_WINDOW_MS);
    await flushPromises();
    assert.equal(world.reports.length, VARIANT_REPORTS_PER_WINDOW + 1);
    assert.deepEqual(world.reports.at(-1)?.description, flips.at(-1));
  });
});

describe("сбой адаптера", () => {
  it("адаптер бросил после непустого меню — фону уходит «меню нет», строка в журнал с паспортом", async () => {
    const world = harness();
    world.watch.arm(LEASE_MS);
    await flushPromises();
    world.fake.state.throws = true;
    world.mutate();
    world.clock.advance(VARIANT_READ_MIN_GAP_MS);
    await flushPromises();
    assert.deepEqual(descriptions(world.reports), [DUB_A, null]);
    assert.deepEqual(world.failures, ["fake@2: Error: adapter broke"]);
  });

  it("адаптер бросил с самого начала — фону ничего: он и не знал о меню", async () => {
    const world = harness();
    world.fake.state.throws = true;
    world.watch.arm(LEASE_MS);
    await flushPromises();
    world.watch.arm(LEASE_MS);
    await flushPromises();
    assert.deepEqual(world.reports, []);
    assert.equal(world.failures.length, 2);
  });

  it("страница сменилась (адаптер больше не подходит) — на продлении уходит «меню нет»", async () => {
    const world = harness();
    world.watch.arm(LEASE_MS);
    await flushPromises();
    world.fake.state.matches = false;
    world.watch.arm(LEASE_MS);
    await flushPromises();
    assert.deepEqual(descriptions(world.reports), [DUB_A, null]);
    assert.equal(world.observers(), 0);
  });
});

describe("команды", () => {
  it("команда доходит до адаптера, исход возвращается как есть; после done меню перечитывается сразу", async () => {
    const world = harness();
    world.watch.arm(LEASE_MS);
    await flushPromises();
    world.fake.state.description = DUB_B;
    assert.equal(await world.watch.command({ kind: "select", variant_kind: "audio", variant_id: "dub-b" }), "done");
    world.clock.advance(VARIANT_READ_MIN_GAP_MS);
    await flushPromises();
    assert.deepEqual(world.fake.commands, [{ kind: "select", variant_kind: "audio", variant_id: "dub-b" }]);
    assert.deepEqual(descriptions(world.reports), [DUB_A, DUB_B]);
    world.fake.state.outcome = "person_only";
    assert.equal(await world.watch.command({ kind: "next" }), "person_only");
  });

  it("адаптер бросил на команде — failed и строка в журнал, исключение наружу не уходит", async () => {
    const world = harness();
    world.watch.arm(LEASE_MS);
    await flushPromises();
    world.fake.state.throws = true;
    assert.equal(await world.watch.command({ kind: "select", variant_kind: "quality", variant_id: "q" }), "failed");
    assert.equal(world.failures.length, 1);
  });

  it("в фрейме нет подходящего адаптера — no_such_variant, никто ничего не нажимал", async () => {
    const world = harness();
    world.fake.state.matches = false;
    assert.equal(await world.watch.command({ kind: "next" }), "no_such_variant");
    assert.deepEqual(world.fake.commands, []);
  });
});

