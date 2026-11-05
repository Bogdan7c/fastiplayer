// Наблюдение за рекламой во фрейме (09d): аренда, истечение, слияние чтений, лимит донесений, скрытая вкладка,
// сбой читателя. Время фейковое; DOM — подделка из tests/adapters.
import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";
import { createYouTubeOrgans } from "../../src/adapters/ad-organs/youtube.ts";
import type { AdElement, AdFramePlace, AdOrgans, PressSkipOutcome } from "../../src/adapters/ad-organs/index.ts";
import type { PageAdState } from "../../src/protocol/index.ts";
import { AD_READ_MIN_GAP_MS, AD_REPORT_WINDOW_MS, AD_REPORTS_PER_WINDOW, createAdWatch, type AdMutationWatcher } from "../../src/content/ad-watch.ts";
import type { AdWatchReport } from "../../src/shared/ad-watch-contract.ts";
import { createFakeClock } from "../background/support.ts";
import { createFakeAdView, FakeNode, youTubePage } from "../adapters/fake-ad-dom.ts";

const SHOWING_ON_PAGE: PageAdState = { kind: "showing", pod: { kind: "known", index: 1, count: 2 }, skip: { kind: "on_page" } };
const SHOWING_COUNTDOWN: PageAdState = { kind: "showing", pod: { kind: "unknown" }, skip: { kind: "countdown", seconds: 5 } };
const NONE: PageAdState = { kind: "none" };
const PLACE: AdFramePlace = { hostname: "example.com", contains: () => false };

/** Читатель, которым управляет тест. */
class ScriptedOrgans implements AdOrgans {
  readonly family = "youtube" as const;
  state: PageAdState = NONE;
  reads = 0;
  failWith: Error | undefined;
  matching = true;
  root: AdElement = new FakeNode("div");
  pressResult: PressSkipOutcome = "no_button";
  matches(): boolean {
    return this.matching;
  }
  observationRoot(): AdElement {
    return this.root;
  }
  readAdState(): PageAdState {
    this.reads += 1;
    if (this.failWith !== undefined) throw this.failWith;
    return this.state;
  }
  pressSkip(): PressSkipOutcome {
    if (this.failWith !== undefined) throw this.failWith;
    return this.pressResult;
  }
}

class FakeWatcher implements AdMutationWatcher {
  observed: AdElement | undefined;
  disconnected = false;
  readonly onMutation: () => void;
  constructor(onMutation: () => void) {
    this.onMutation = onMutation;
  }
  observe(root: AdElement): void {
    this.observed = root;
  }
  disconnect(): void {
    this.disconnected = true;
  }
}

function setup(organs: readonly AdOrgans[] = [new ScriptedOrgans()]) {
  const clock = createFakeClock();
  const reports: { readonly at: number; readonly ad: PageAdState }[] = [];
  const failures: unknown[] = [];
  const watchers: FakeWatcher[] = [];
  const watch = createAdWatch({
    organs,
    place: PLACE,
    createWatcher: (onMutation) => {
      const watcher = new FakeWatcher(onMutation);
      watchers.push(watcher);
      return watcher;
    },
    timers: clock.scheduler,
    now: () => clock.scheduler.now(),
    sendReport: (report: AdWatchReport) => void reports.push({ at: clock.scheduler.now(), ad: report.ad }),
    reportReaderFailure: (error) => void failures.push(error),
  });
  /** Живые (не отключённые) наблюдатели. */
  const liveWatchers = (): FakeWatcher[] => watchers.filter((watcher) => !watcher.disconnected);
  return { clock, reports, failures, watchers, liveWatchers, watch };
}

describe("аренда наблюдения", () => {
  it("до аренды нет ни наблюдателей, ни таймеров, ни донесений (решение 198)", () => {
    const organs = new ScriptedOrgans();
    organs.state = SHOWING_ON_PAGE;
    const world = setup([organs]);
    world.clock.advance(60_000);
    assert.equal(world.watchers.length, 0);
    assert.equal(world.clock.liveTimers(), 0);
    assert.equal(world.reports.length, 0);
    assert.equal(organs.reads, 0);
    assert.equal(world.watch.isWatching(), false);
  });

  it("аренда ставит наблюдателя на корень читателя и сразу читает: идущая реклама донесена", () => {
    const organs = new ScriptedOrgans();
    organs.state = SHOWING_ON_PAGE;
    const world = setup([organs]);
    world.watch.arm(3_000);
    assert.equal(world.liveWatchers().length, 1);
    assert.equal(world.liveWatchers()[0]?.observed, organs.root);
    assert.deepEqual(world.reports.map((report) => report.ad), [SHOWING_ON_PAGE]);
  });

  it("истечение аренды снимает наблюдателя и таймеры; мутации после этого ничего не читают", () => {
    const organs = new ScriptedOrgans();
    const world = setup([organs]);
    world.watch.arm(3_000);
    const watcher = world.liveWatchers()[0];
    world.clock.advance(2_999);
    assert.equal(world.watch.isWatching(), true);
    world.clock.advance(1);
    assert.equal(world.watch.isWatching(), false);
    assert.equal(watcher?.disconnected, true);
    assert.equal(world.clock.liveTimers(), 0);
    const readsBefore = organs.reads;
    organs.state = SHOWING_ON_PAGE;
    watcher?.onMutation();
    world.clock.advance(1_000);
    assert.equal(organs.reads, readsBefore);
    assert.equal(world.reports.length, 0);
  });

  it("продление сдвигает срок: наблюдатель живёт, пока фон продлевает", () => {
    const world = setup();
    world.watch.arm(3_000);
    for (let second = 0; second < 10; second += 1) {
      world.clock.advance(1_000);
      world.watch.arm(3_000);
    }
    assert.equal(world.watch.isWatching(), true);
    assert.equal(world.liveWatchers().length, 1);
    assert.equal(world.watchers.length, 1, "наблюдатель не пересоздаётся без причины");
    world.clock.advance(3_000);
    assert.equal(world.watch.isWatching(), false);
  });

  it("фрейм, которому читать нечего, аренду не держит", () => {
    const organs = new ScriptedOrgans();
    organs.matching = false;
    const world = setup([organs]);
    world.watch.arm(3_000);
    assert.equal(world.watchers.length, 0);
    assert.equal(world.watch.isWatching(), false);
    assert.equal(world.clock.liveTimers(), 0);
    assert.equal(organs.reads, 0);
  });

  it("корень сменился (страница перестроила плеер) — на продлении наблюдатель переезжает на новый корень", () => {
    const organs = new ScriptedOrgans();
    const world = setup([organs]);
    world.watch.arm(3_000);
    const first = world.liveWatchers()[0];
    organs.root = new FakeNode("div");
    world.watch.arm(3_000);
    assert.equal(first?.disconnected, true);
    assert.equal(world.liveWatchers().length, 1);
    assert.equal(world.liveWatchers()[0]?.observed, organs.root);
  });

  it("после истечения новая аренда начинает с чистого листа: идущая реклама донесена снова", () => {
    const organs = new ScriptedOrgans();
    organs.state = SHOWING_ON_PAGE;
    const world = setup([organs]);
    world.watch.arm(1_000);
    world.clock.advance(1_000);
    world.watch.arm(1_000);
    assert.equal(world.reports.length, 2);
  });
});

describe("донесения", () => {
  it("вне рекламы мутации страницу не читают: фильм под тенью не стоит запросов раскладки", () => {
    const organs = new ScriptedOrgans();
    const world = setup([organs]);
    world.watch.arm(3_000);
    const readsAfterArm = organs.reads;
    world.clock.advance(500);
    for (let mutation = 0; mutation < 20; mutation += 1) {
      world.liveWatchers()[0]?.onMutation();
      world.clock.advance(AD_READ_MIN_GAP_MS);
    }
    assert.equal(organs.reads, readsAfterArm, "мутации не читали");
    assert.equal(world.reports.length, 0);
    // Начало рекламы замечает продление аренды.
    organs.state = SHOWING_COUNTDOWN;
    world.watch.arm(3_000);
    assert.deepEqual(world.reports.map((report) => report.ad), [SHOWING_COUNTDOWN]);
  });

  it("в рекламе изменение по мутации уходит сразу, без дублей и без донесений «рекламы нет» из тишины", () => {
    const organs = new ScriptedOrgans();
    const world = setup([organs]);
    world.watch.arm(3_000);
    assert.equal(world.reports.length, 0, "рекламы нет — молчим");
    world.clock.advance(500);
    organs.state = SHOWING_COUNTDOWN;
    world.watch.arm(3_000);
    assert.deepEqual(world.reports.map((report) => report.ad), [SHOWING_COUNTDOWN]);
    // Мутация без смены состояния ничего не шлёт.
    world.clock.advance(AD_READ_MIN_GAP_MS);
    world.liveWatchers()[0]?.onMutation();
    assert.equal(world.reports.length, 1);
    organs.state = NONE;
    world.clock.advance(AD_READ_MIN_GAP_MS);
    world.liveWatchers()[0]?.onMutation();
    assert.deepEqual(world.reports.map((report) => report.ad), [SHOWING_COUNTDOWN, NONE]);
  });

  it("на каждом продлении, пока реклама идёт, донесение повторяется (фон считает молчание концом)", () => {
    const organs = new ScriptedOrgans();
    organs.state = SHOWING_ON_PAGE;
    const world = setup([organs]);
    for (let renewal = 0; renewal < 4; renewal += 1) {
      world.watch.arm(3_000);
      world.clock.advance(1_000);
    }
    assert.equal(world.reports.length, 4);
  });

  it("продление ловит перемену, которую наблюдатель пропустил (страховка)", () => {
    const organs = new ScriptedOrgans();
    const world = setup([organs]);
    world.watch.arm(3_000);
    organs.state = SHOWING_ON_PAGE; // мутации нет: наблюдатель «проспал»
    world.clock.advance(1_000);
    world.watch.arm(3_000);
    assert.deepEqual(world.reports.map((report) => report.ad), [SHOWING_ON_PAGE]);
  });

  it("продление при отсутствии рекламы молчит", () => {
    const world = setup();
    for (let renewal = 0; renewal < 5; renewal += 1) {
      world.watch.arm(3_000);
      world.clock.advance(1_000);
    }
    assert.equal(world.reports.length, 0);
  });
});

describe("слияние чтений", () => {
  it("всплеск мутаций читается не чаще раза в окно, а последняя перемена не теряется (хвостовое чтение)", () => {
    const organs = new ScriptedOrgans();
    organs.state = SHOWING_ON_PAGE;
    const world = setup([organs]);
    world.watch.arm(60_000);
    world.clock.advance(1_000);
    const readsBefore = organs.reads;
    const watcher = world.liveWatchers()[0];
    // 50 мутаций за 49 мс; состояние меняется на каждой, финальное — отсчёт.
    for (let mutation = 0; mutation < 50; mutation += 1) {
      organs.state = mutation === 49 ? SHOWING_COUNTDOWN : mutation % 2 === 0 ? SHOWING_ON_PAGE : SHOWING_COUNTDOWN;
      watcher?.onMutation();
      world.clock.advance(1);
    }
    world.clock.advance(AD_READ_MIN_GAP_MS);
    assert.equal(organs.reads - readsBefore, 2, "одно чтение сразу и одно хвостовое");
    assert.deepEqual(world.reports.at(-1)?.ad, SHOWING_COUNTDOWN, "хвостовое чтение увидело последнее состояние");
  });

  it("одиночная мутация сразу после чтения читается не раньше конца окна, но обязательно читается", () => {
    const organs = new ScriptedOrgans();
    organs.state = SHOWING_COUNTDOWN;
    const world = setup([organs]);
    world.watch.arm(60_000);
    world.clock.advance(1_000);
    const watcher = world.liveWatchers()[0];
    watcher?.onMutation(); // читаем сразу
    const readsAfterFirst = organs.reads;
    world.clock.advance(10);
    organs.state = SHOWING_ON_PAGE;
    watcher?.onMutation();
    assert.equal(organs.reads, readsAfterFirst, "внутри окна не читаем");
    assert.equal(world.reports.length, 1, "только донесение аренды");
    world.clock.advance(AD_READ_MIN_GAP_MS);
    assert.equal(organs.reads, readsAfterFirst + 1);
    assert.deepEqual(world.reports.map((report) => report.ad), [SHOWING_COUNTDOWN, SHOWING_ON_PAGE]);
  });
});

describe("лимит донесений", () => {
  it("страница, мигающая состоянием, даёт не больше 8 донесений в окно; лишние сливаются в последнее", () => {
    const organs = new ScriptedOrgans();
    organs.state = SHOWING_COUNTDOWN;
    const world = setup([organs]);
    world.watch.arm(600_000);
    const watcher = world.liveWatchers()[0];
    // Каждые 100 мс (предел слияния) состояние иное: попыток 10 в секунду.
    for (let tick = 0; tick < 50; tick += 1) {
      organs.state = tick % 2 === 0 ? SHOWING_ON_PAGE : SHOWING_COUNTDOWN;
      watcher?.onMutation();
      world.clock.advance(AD_READ_MIN_GAP_MS);
    }
    for (const report of world.reports) {
      const inWindow = world.reports.filter((other) => other.at >= report.at && other.at < report.at + AD_REPORT_WINDOW_MS).length;
      assert.ok(inWindow <= AD_REPORTS_PER_WINDOW, `в окне с ${report.at} мс ушло ${inWindow} донесений`);
    }
    assert.ok(world.reports.length > AD_REPORTS_PER_WINDOW, "донесения всё же идут, а не замолкают");
    // Страница успокоилась: фон узнает ИТОГОВОЕ состояние, а не потерянное сливом.
    organs.state = NONE;
    watcher?.onMutation();
    world.clock.advance(AD_REPORT_WINDOW_MS * 2);
    assert.deepEqual(world.reports.at(-1)?.ad, NONE);
  });
});

describe("скрытая вкладка", () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, "requestAnimationFrame");
  afterEach(() => {
    if (original === undefined) Reflect.deleteProperty(globalThis, "requestAnimationFrame");
    else Object.defineProperty(globalThis, "requestAnimationFrame", original);
  });

  it("наблюдение не опирается на requestAnimationFrame: в скрытой вкладке он не вызывается, а донесения идут", () => {
    // Скрытая вкладка: кадров нет. Любое обращение к rAF здесь — провал.
    Object.defineProperty(globalThis, "requestAnimationFrame", {
      configurable: true,
      value: () => {
        throw new Error("requestAnimationFrame must not be used");
      },
    });
    const organs = new ScriptedOrgans();
    organs.state = SHOWING_ON_PAGE;
    const world = setup([organs]);
    world.watch.arm(3_000);
    world.liveWatchers()[0]?.onMutation();
    world.clock.advance(AD_READ_MIN_GAP_MS);
    organs.state = SHOWING_COUNTDOWN;
    world.liveWatchers()[0]?.onMutation();
    world.clock.advance(AD_READ_MIN_GAP_MS * 2);
    assert.deepEqual(world.reports.at(-1)?.ad, SHOWING_COUNTDOWN);
  });
});

describe("сбой читателя не ошибка передачи", () => {
  it("исключение читателя ловится на границе: ни броска наружу, ни донесения, сбой записан", () => {
    const organs = new ScriptedOrgans();
    organs.failWith = new Error("page broke the DOM");
    const world = setup([organs]);
    assert.doesNotThrow(() => world.watch.arm(3_000));
    assert.doesNotThrow(() => world.liveWatchers()[0]?.onMutation());
    assert.equal(world.reports.length, 0);
    assert.ok(world.failures.length >= 1);
  });

  it("после сбоя читатель продолжает работать, а сломанный не мешает соседнему", () => {
    const broken = new ScriptedOrgans();
    broken.failWith = new Error("boom");
    const healthy = new ScriptedOrgans();
    healthy.state = SHOWING_ON_PAGE;
    const world = setup([broken, healthy]);
    world.watch.arm(3_000);
    assert.deepEqual(world.reports.map((report) => report.ad), [SHOWING_ON_PAGE]);
    broken.failWith = undefined;
    broken.state = SHOWING_COUNTDOWN;
    healthy.state = NONE;
    world.clock.advance(1_000);
    world.watch.arm(3_000);
    assert.deepEqual(world.reports.at(-1)?.ad, SHOWING_COUNTDOWN);
  });

  it("сбой pressSkip — no_button и запись сбоя, не исключение", () => {
    const organs = new ScriptedOrgans();
    organs.failWith = new Error("boom");
    const world = setup([organs]);
    assert.equal(world.watch.pressSkip(), "no_button");
    assert.equal(world.failures.length, 1);
  });
});

describe("pressSkip", () => {
  it("нажатие доходит до читателя и итог возвращается; без аренды тоже работает", () => {
    const organs = new ScriptedOrgans();
    organs.pressResult = "pressed";
    const world = setup([organs]);
    assert.equal(world.watch.pressSkip(), "pressed");
  });

  it("подходящие читатели опрашиваются по очереди: pressed сильнее person_only, person_only сильнее no_button", () => {
    const first = new ScriptedOrgans();
    const second = new ScriptedOrgans();
    first.pressResult = "person_only";
    second.pressResult = "no_button";
    assert.equal(setup([first, second]).watch.pressSkip(), "person_only");
    second.pressResult = "pressed";
    assert.equal(setup([first, second]).watch.pressSkip(), "pressed");
    first.pressResult = "no_button";
    second.pressResult = "no_button";
    assert.equal(setup([first, second]).watch.pressSkip(), "no_button");
  });

  it("неподходящий фрейму читатель не нажимает ничего", () => {
    const organs = new ScriptedOrgans();
    organs.matching = false;
    organs.pressResult = "pressed";
    assert.equal(setup([organs]).watch.pressSkip(), "no_button");
  });
});

describe("со настоящим читателем YouTube", () => {
  it("класс ad-showing и показанная кнопка доходят до донесения on_page; уход рекламы — до none", () => {
    const page = youTubePage({ adShowing: false, counterText: "1 из 2", skip: "hidden" });
    const view = createFakeAdView(page.root, { hostname: "www.youtube.com" });
    const clock = createFakeClock();
    const reports: PageAdState[] = [];
    const watchers: FakeWatcher[] = [];
    const watch = createAdWatch({
      organs: [createYouTubeOrgans(view)],
      place: view,
      createWatcher: (onMutation) => {
        const watcher = new FakeWatcher(onMutation);
        watchers.push(watcher);
        return watcher;
      },
      timers: clock.scheduler,
      now: () => clock.scheduler.now(),
      sendReport: (report) => void reports.push(report.ad),
      reportReaderFailure: (error) => assert.fail(String(error)),
    });
    watch.arm(3_000);
    assert.equal(watchers[0]?.observed, page.player, "следим за плеером, а не за всем документом");
    clock.advance(AD_READ_MIN_GAP_MS);
    page.player.classes.add("ad-showing");
    // Начало рекламы замечает продление аренды, дальше работает наблюдатель.
    watch.arm(3_000);
    page.skipButton!.rendered = true;
    clock.advance(AD_READ_MIN_GAP_MS);
    watchers[0]?.onMutation();
    page.player.classes.delete("ad-showing");
    clock.advance(AD_READ_MIN_GAP_MS);
    watchers[0]?.onMutation();
    assert.deepEqual(reports, [
      { kind: "showing", pod: { kind: "known", index: 1, count: 2 }, skip: { kind: "unavailable" } },
      { kind: "showing", pod: { kind: "known", index: 1, count: 2 }, skip: { kind: "on_page" } },
      { kind: "none" },
    ]);
  });
});
