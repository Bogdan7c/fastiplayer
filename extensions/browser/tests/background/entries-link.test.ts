// Вход «правый клик по ссылке» (сессия 10c, решение 217): вкладка спереди, ожидание видео, закрытие концом сеанса.
// Часть 1 — вход на фейковых командах передачи (время, опрос, срок). Часть 2 — вход на НАСТОЯЩЕМ владельце
// сеансов (`createTabHandoffs` на фейках): закрытие именно по концу сеанса, замена и «страница забрала» не закрывают.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createLinkEntry, LINK_POLL_INTERVAL_MS, LINK_VIDEO_WAIT_MS, type LinkEntry, type LinkHandoffs, type LinkTabsApi } from "../../src/background/entries/link-entry.ts";
import type { EntryResult } from "../../src/background/entries/entry-report-text.ts";
import type { EntryReporter } from "../../src/background/entries/entry-reporter.ts";
import type { HandoffSessionEnd } from "../../src/background/handoff/session-contract.ts";
import type { SessionCloseReason } from "../../src/protocol/session/close-reason.ts";
import type { HandOffTabOutcome, VideoCandidate, VideoRef } from "../../src/shared/handoff-contract.ts";
import type { SurveyTabReply } from "../../src/shared/survey-contract.ts";
import type { TabVideoRecord } from "../../src/background/tab-model/tab-snapshot.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { PLAYING_VIDEO, setup, TAB, type World } from "./handoff-world.ts";
import { createFakeClock, flushPromises, type FakeClock } from "./support.ts";

const SOURCE = { tabId: 3, windowId: 1, index: 5 };
const LINK_TAB = 40;
const STARTED: HandOffTabOutcome = { kind: "started", player_window: "new" };
const REF: VideoRef = { frame_id: 0, document: "doc-1", element: 1 };
const SURVEY_NO_VIDEO: SurveyTabReply = { kind: "surveyed", videos: [], main: null, in_player: { kind: "no_session" } };
const recordOf = (playing: boolean): TabVideoRecord => ({
  ...DEFAULT_CANDIDATE_FACTS,
  document: REF.document,
  element: REF.element,
  frame_id: REF.frame_id,
  media: { kind: "media_source", in_worker: false },
  playing,
  visible_area_px: 1000,
  verdict: "main",
});
/** Главное видео ИГРАЕТ: вход по ссылке его берёт. */
const SURVEY_WITH_VIDEO: SurveyTabReply = { kind: "surveyed", videos: [recordOf(true)], main: REF, in_player: { kind: "no_session" } };
/** Главное видео стоит на паузе: попап его бы отдал, ссылка ждёт play (решение 217). */
const SURVEY_PAUSED_VIDEO: SurveyTabReply = { kind: "surveyed", videos: [recordOf(false)], main: REF, in_player: { kind: "no_session" } };
const SURVEY_UNAVAILABLE: SurveyTabReply = { kind: "unavailable", reason: "tab_not_scriptable" };

/** Один шаг ожидания входа: часы вперёд на период опроса и дать отработать цепочке промисов. */
async function tick(clock: FakeClock, ms = LINK_POLL_INTERVAL_MS): Promise<void> {
  clock.advance(ms);
  await flushPromises();
}

interface TabsLog {
  readonly opened: unknown[];
  readonly activated: number[];
  readonly closed: number[];
  readonly order: string[];
}

function fakeTabs(options: { createFails?: boolean; activateFails?: boolean; closeFails?: boolean; activeTab?: number; tabGone?: boolean } = {}): { api: LinkTabsApi; log: TabsLog } {
  const log: TabsLog = { opened: [], activated: [], closed: [], order: [] };
  const api: LinkTabsApi = {
    openActiveTab: (properties) => {
      if (options.createFails === true) return Promise.reject(new Error("could not create"));
      log.opened.push(properties);
      return Promise.resolve(LINK_TAB);
    },
    activateTab: (tabId) => {
      log.order.push(`activate ${tabId}`);
      if (options.activateFails === true) return Promise.reject(new Error("No tab with id"));
      log.activated.push(tabId);
      return Promise.resolve();
    },
    // По умолчанию человек смотрит на закрываемую вкладку.
    isTabActive: (tabId) => Promise.resolve((options.activeTab ?? LINK_TAB) === tabId),
    tabExists: () => Promise.resolve(options.tabGone !== true),
    closeTab: (tabId) => {
      log.order.push(`close ${tabId}`);
      if (options.closeFails === true) return Promise.reject(new Error("No tab with id"));
      log.closed.push(tabId);
      return Promise.resolve();
    },
  };
  return { api, log };
}

function fakeReporter() {
  const reports: { tabId: number; result: EntryResult }[] = [];
  const reporter: EntryReporter = { report: (tabId, result) => (reports.push({ tabId, result }), Promise.resolve()), forgetProblem: () => Promise.resolve() };
  return { reporter, reports };
}

/** Вход на скриптованных опросах и исходах; `active` — вкладки, у которых «сеанс идёт». */
function setupScripted(options: { surveys?: (SurveyTabReply | Promise<SurveyTabReply>)[]; outcomes?: HandOffTabOutcome[]; tabsOptions?: Parameters<typeof fakeTabs>[0] } = {}) {
  const clock = createFakeClock();
  const { api, log } = fakeTabs(options.tabsOptions);
  const { reporter, reports } = fakeReporter();
  const surveys = [...(options.surveys ?? [])];
  const outcomes = [...(options.outcomes ?? [])];
  const active = new Set<number>();
  const notes: string[] = [];
  let surveyCalls = 0;
  let handOffCalls = 0;
  const handoffs: LinkHandoffs = {
    surveyTab: () => {
      surveyCalls += 1;
      return Promise.resolve(surveys.length > 1 ? (surveys.shift() as SurveyTabReply) : (surveys[0] ?? SURVEY_NO_VIDEO));
    },
    handOffTab: () => {
      handOffCalls += 1;
      return Promise.resolve(outcomes.shift() ?? STARTED);
    },
    activeTabIds: () => [...active],
  };
  const entry = createLinkEntry({ tabs: api, handoffs, scheduler: clock.scheduler, reporter, note: (message) => void notes.push(message) });
  return { entry, clock, log, reports, active, notes, surveyCalls: () => surveyCalls, handOffCalls: () => handOffCalls };
}

describe("вход по ссылке: открытие и ожидание", () => {
  it("открывает активную вкладку рядом с исходной (за ней), с openerTabId и окном исходной", async () => {
    const world = setupScripted();
    void world.entry.openLink("https://site.example/watch", SOURCE);
    await flushPromises();
    assert.deepEqual(world.log.opened, [{ url: "https://site.example/watch", openerTabId: 3, windowId: 1, index: 6 }]);
    assert.deepEqual(world.entry.ownedTabIds(), [LINK_TAB]);
    assert.equal(world.surveyCalls(), 0, "до первого периода опроса не спрашиваем");
  });

  it("сбой tabs.create: плашка на ИСХОДНОЙ вкладке, ничего своим не считается", async () => {
    const world = setupScripted({ tabsOptions: { createFails: true } });
    await world.entry.openLink("https://site.example/", SOURCE);
    assert.deepEqual(world.reports, [{ tabId: 3, result: { kind: "link_not_opened" } }]);
    assert.deepEqual(world.entry.ownedTabIds(), []);
    assert.equal(world.notes.length, 1);
  });

  it("не веб-адрес (file:, javascript:, мусор, нет адреса): вкладка не открывается, плашка на исходной", async () => {
    const world = setupScripted();
    for (const address of ["file:///home/me/movie.mp4", "javascript:alert(1)", "not a url", undefined]) {
      await world.entry.openLink(address, SOURCE);
    }
    assert.deepEqual(world.log.opened, []);
    assert.deepEqual(world.reports.map((report) => [report.tabId, report.result.kind]), Array.from({ length: 4 }, () => [3, "link_not_opened"]));
  });

  it("опрос раз в период и только после завершения предыдущего", async () => {
    let finishSurvey: (reply: SurveyTabReply) => void = () => {};
    const slow = new Promise<SurveyTabReply>((resolve) => (finishSurvey = resolve));
    const world = setupScripted({ surveys: [slow, SURVEY_NO_VIDEO] });
    void world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    assert.equal(world.surveyCalls(), 1);
    await tick(world.clock, LINK_POLL_INTERVAL_MS * 5);
    assert.equal(world.surveyCalls(), 1, "пока первый опрос не вернулся, второй не начинается");
    finishSurvey(SURVEY_NO_VIDEO);
    await flushPromises();
    await tick(world.clock);
    assert.equal(world.surveyCalls(), 2);
  });

  it("страница ещё грузится (unavailable) и видео нет — ждём; видео появилось — передача, вкладка остаётся нашей", async () => {
    const world = setupScripted({ surveys: [SURVEY_UNAVAILABLE, SURVEY_NO_VIDEO, SURVEY_WITH_VIDEO], outcomes: [STARTED] });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    await tick(world.clock);
    assert.equal(world.handOffCalls(), 0, "без видео не передаём");
    await tick(world.clock);
    await done;
    assert.equal(world.handOffCalls(), 1);
    assert.deepEqual(world.reports, [{ tabId: LINK_TAB, result: { kind: "outcome", outcome: STARTED } }]);
    assert.deepEqual(world.entry.ownedTabIds(), [LINK_TAB], "вкладка за входом до конца сеанса");
    assert.deepEqual(world.log.closed, []);
  });

  it("главное видео стоит на паузе: вход по ссылке НЕ передаёт, ждёт play; после play — передача", async () => {
    const world = setupScripted({ surveys: [SURVEY_PAUSED_VIDEO, SURVEY_PAUSED_VIDEO, SURVEY_WITH_VIDEO], outcomes: [STARTED] });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    await tick(world.clock);
    assert.equal(world.handOffCalls(), 0, "на паузе не берём");
    await tick(world.clock);
    await done;
    assert.equal(world.handOffCalls(), 1);
    assert.deepEqual(world.reports, [{ tabId: LINK_TAB, result: { kind: "outcome", outcome: STARTED } }]);
  });

  it("видео так и стоит на паузе до срока: обычная плашка срока, вкладка не закрывается", async () => {
    const world = setupScripted({ surveys: [SURVEY_PAUSED_VIDEO] });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    for (let waited = 0; waited < LINK_VIDEO_WAIT_MS; waited += LINK_POLL_INTERVAL_MS) await tick(world.clock);
    await done;
    assert.equal(world.handOffCalls(), 0);
    assert.deepEqual(world.reports, [{ tabId: LINK_TAB, result: { kind: "link_no_video_started" } }]);
    assert.deepEqual(world.log.closed, []);
  });

  it("временные отказы (video_gone, failed survey_timed_out / tab_not_scriptable) — ждём дальше; окончательные failed и player_unavailable — плашка", async () => {
    const temporary: HandOffTabOutcome[] = [
      { kind: "video_gone" },
      { kind: "failed", reason: "survey_timed_out" },
      { kind: "failed", reason: "tab_not_scriptable:no frame accepted the injection" },
    ];
    const waiting = setupScripted({ surveys: [SURVEY_WITH_VIDEO], outcomes: [...temporary, STARTED] });
    const done = waiting.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    for (let step = 0; step < 4; step += 1) await tick(waiting.clock);
    await done;
    assert.equal(waiting.handOffCalls(), 4, "три временных отказа пережиты");
    assert.deepEqual(waiting.reports, [{ tabId: LINK_TAB, result: { kind: "outcome", outcome: STARTED } }]);

    for (const final of [{ kind: "failed", reason: "internal:boom" }, { kind: "player_unavailable", reason: "no_reply" }] as const) {
      const refused = setupScripted({ surveys: [SURVEY_WITH_VIDEO], outcomes: [final] });
      const finished = refused.entry.openLink("https://site.example/", SOURCE);
      await flushPromises();
      await tick(refused.clock);
      await finished;
      assert.deepEqual(refused.reports, [{ tabId: LINK_TAB, result: { kind: "outcome", outcome: final } }], final.kind);
      assert.deepEqual(refused.entry.ownedTabIds(), []);
    }
  });

  it("вкладку закрыли до того, как вход её запомнил: при unavailable вход видит, что вкладки нет, и тихо заканчивает — без плашки и без минуты опросов", async () => {
    const world = setupScripted({ surveys: [SURVEY_UNAVAILABLE], tabsOptions: { tabGone: true } });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    await done;
    assert.deepEqual(world.reports, []);
    assert.deepEqual(world.entry.ownedTabIds(), []);
    assert.equal(world.surveyCalls(), 1);
    assert.equal(world.clock.liveTimers(), 0);
  });

  it("no_video от передачи (видео пропало между опросом и передачей) — ждём дальше, а не сдаёмся", async () => {
    const world = setupScripted({ surveys: [SURVEY_WITH_VIDEO], outcomes: [{ kind: "no_video" }, { kind: "no_video" }, STARTED] });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    await tick(world.clock);
    assert.deepEqual(world.reports, [], "отказом это не считается");
    await tick(world.clock);
    await done;
    assert.equal(world.handOffCalls(), 3);
    assert.deepEqual(world.reports, [{ tabId: LINK_TAB, result: { kind: "outcome", outcome: STARTED } }]);
  });

  it("человек сам нажал значок (видео уже в плеере): повторной передачи нет, вкладка остаётся нашей до конца сеанса", async () => {
    const world = setupScripted({ surveys: [{ ...SURVEY_WITH_VIDEO, in_player: { kind: "in_player", video: REF } }] });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    await done;
    assert.equal(world.handOffCalls(), 0);
    world.active.add(LINK_TAB);
    world.active.delete(LINK_TAB);
    world.entry.onSessionEnded(LINK_TAB, { kind: "returned_to_page" });
    await flushPromises();
    assert.deepEqual(world.log.closed, [LINK_TAB]);
  });

  it("срок ожидания вышел: плашка «видео не запустилось» на ЭТОЙ вкладке, вкладка НЕ закрывается и больше не наша", async () => {
    const world = setupScripted({ surveys: [SURVEY_NO_VIDEO] });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    for (let waited = 0; waited < LINK_VIDEO_WAIT_MS; waited += LINK_POLL_INTERVAL_MS) await tick(world.clock);
    await done;
    assert.deepEqual(world.reports, [{ tabId: LINK_TAB, result: { kind: "link_no_video_started" } }]);
    assert.deepEqual(world.log.closed, []);
    assert.deepEqual(world.entry.ownedTabIds(), []);
    assert.equal(world.clock.liveTimers(), 0, "опрос остановлен");
    const surveysAtTimeout = world.surveyCalls();
    await tick(world.clock, LINK_VIDEO_WAIT_MS);
    assert.equal(world.surveyCalls(), surveysAtTimeout, "после срока опросов больше нет");
    world.entry.onSessionEnded(LINK_TAB, { kind: "returned_to_page" });
    await flushPromises();
    assert.deepEqual(world.log.closed, [], "вкладка стала человеческой: даже поздний конец сеанса её не закрывает");
  });

  it("окончательный отказ передачи (защищённое видео): плашка с причиной на вкладке, вкладка не закрывается и не наша", async () => {
    const world = setupScripted({ surveys: [SURVEY_WITH_VIDEO], outcomes: [{ kind: "protected" }] });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    await done;
    assert.deepEqual(world.reports, [{ tabId: LINK_TAB, result: { kind: "outcome", outcome: { kind: "protected" } } }]);
    assert.deepEqual(world.log.closed, []);
    assert.deepEqual(world.entry.ownedTabIds(), []);
  });

  it("сбой опроса (исключение) ожидание не прерывает: следующий период опрашивает снова и доводит до передачи", async () => {
    const clock = createFakeClock();
    const { reporter, reports } = fakeReporter();
    const notes: string[] = [];
    let surveyCalls = 0;
    const entry = createLinkEntry({
      tabs: fakeTabs().api,
      handoffs: {
        surveyTab: () => ((surveyCalls += 1) === 1 ? Promise.reject(new Error("boom")) : Promise.resolve(SURVEY_WITH_VIDEO)),
        handOffTab: () => Promise.resolve(STARTED),
        activeTabIds: () => [],
      },
      scheduler: clock.scheduler,
      reporter,
      note: (message) => void notes.push(message),
    });
    const done = entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(clock);
    await tick(clock);
    await done;
    assert.equal(surveyCalls, 2);
    assert.equal(notes.length, 1, "сбой отмечен для разработчика");
    assert.deepEqual(reports, [{ tabId: LINK_TAB, result: { kind: "outcome", outcome: STARTED } }]);
  });
});

describe("вход по ссылке: tabs.onRemoved", () => {
  it("человек закрыл вкладку во время ожидания: опросы прекращаются сразу, сообщений нет, вкладка забыта", async () => {
    const world = setupScripted({ surveys: [SURVEY_NO_VIDEO] });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    assert.equal(world.surveyCalls(), 1);
    world.entry.onTabRemoved(LINK_TAB);
    await flushPromises();
    await done;
    assert.deepEqual(world.entry.ownedTabIds(), []);
    assert.equal(world.clock.liveTimers(), 0, "спящий опрос разбужен и снят, а не ждёт секунду");
    await tick(world.clock, LINK_VIDEO_WAIT_MS);
    assert.equal(world.surveyCalls(), 1);
    assert.deepEqual(world.reports, []);
  });

  it("чужая вкладка (не открытая входом) ничего не меняет", async () => {
    const world = setupScripted({ surveys: [SURVEY_NO_VIDEO] });
    void world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    world.entry.onTabRemoved(999);
    assert.deepEqual(world.entry.ownedTabIds(), [LINK_TAB]);
  });
});

describe("вход по ссылке: конец сеанса", () => {
  /** Вход, у которого вкладка уже в сеансе (передача состоялась). */
  async function startedEntry(tabsOptions: Parameters<typeof fakeTabs>[0] = {}) {
    const world = setupScripted({ surveys: [SURVEY_WITH_VIDEO], outcomes: [STARTED], tabsOptions });
    const done = world.entry.openLink("https://site.example/", SOURCE);
    await flushPromises();
    await tick(world.clock);
    await done;
    world.active.add(LINK_TAB);
    return world;
  }
  const endOfSession = (world: Awaited<ReturnType<typeof startedEntry>>, end: HandoffSessionEnd): Promise<void> => {
    world.active.delete(LINK_TAB);
    world.entry.onSessionEnded(LINK_TAB, end);
    return flushPromises();
  };

  it("нормальный конец со стороны плеера (returned_to_page; окно закрыто, заменено, плеер выключается; чистое закрытие 1000): человек возвращается на исходную вкладку, потом вкладка входа закрывается", async () => {
    const closed = (code: number, reason: SessionCloseReason | undefined): HandoffSessionEnd => ({ kind: "player_ended", cause: { kind: "player_closed", code, reason } });
    const normalEnds: HandoffSessionEnd[] = [
      { kind: "returned_to_page" },
      closed(4000, "player_closed"),
      closed(4001, "replaced_by_another_tab"),
      closed(4006, "player_shutting_down"),
      closed(1000, undefined),
    ];
    for (const end of normalEnds) {
      const world = await startedEntry();
      await endOfSession(world, end);
      assert.deepEqual(world.log.order, [`activate ${SOURCE.tabId}`, `close ${LINK_TAB}`], end.kind);
      assert.deepEqual(world.entry.ownedTabIds(), []);
    }
  });

  it("любой другой конец (страница забрала, ушла, сбой, отказ старта, авария плеера или сокета) — вкладка уже человека: не закрывается, но забывается", async () => {
    const ends: HandoffSessionEnd[] = [
      // «Return» с заглушки (решение 236): видео играет дальше на этой вкладке.
      { kind: "returned_by_viewer" },
      { kind: "page_gone", reason: "page_player_failed" },
      { kind: "page_took_video_back" },
      { kind: "page_gone", reason: "navigated_away" },
      { kind: "page_side_failure" },
      { kind: "shadow_silent" },
      { kind: "page_flood", overBudget: "mse" },
      { kind: "local_address_refused" },
      { kind: "stream_rejected" },
      { kind: "start_failed", outcome: { kind: "no_video" } },
      // Авария плеера или сокета: видео вернулось на страницу, вкладку закрывать нельзя.
      { kind: "player_ended", cause: { kind: "queue_overflow" } },
      { kind: "player_ended", cause: { kind: "attach_timeout" } },
      { kind: "player_ended", cause: { kind: "player_protocol_violation", detail: "x" } },
      { kind: "player_ended", cause: { kind: "page_message_rejected", detail: "x" } },
      { kind: "player_ended", cause: { kind: "player_closed", code: 1006, reason: undefined } },
      { kind: "player_ended", cause: { kind: "player_closed", code: 4003, reason: "protocol_violation" } },
      { kind: "player_ended", cause: { kind: "player_closed", code: 4999, reason: undefined } },
    ];
    for (const end of ends) {
      const world = await startedEntry();
      await endOfSession(world, end);
      assert.deepEqual(world.log.order, [], end.kind);
      assert.deepEqual(world.entry.ownedTabIds(), [], `${end.kind}: забыта`);
    }
  });

  it("замена видео на той же вкладке (сеанс продолжается — вкладка среди активных): ничего не делаем, закроется концом нового сеанса", async () => {
    const world = await startedEntry();
    world.entry.onSessionEnded(LINK_TAB, { kind: "returned_to_page" }); // active всё ещё содержит вкладку
    await flushPromises();
    assert.deepEqual(world.log.order, []);
    assert.deepEqual(world.entry.ownedTabIds(), [LINK_TAB]);
    await endOfSession(world, { kind: "returned_to_page" });
    assert.deepEqual(world.log.closed, [LINK_TAB]);
  });

  it("закрывается только своя вкладка: конец сеанса обычной вкладки не закрывает ничего", async () => {
    const world = await startedEntry();
    world.entry.onSessionEnded(77, { kind: "returned_to_page" });
    await flushPromises();
    assert.deepEqual(world.log.order, []);
    assert.deepEqual(world.entry.ownedTabIds(), [LINK_TAB]);
  });

  it("закрытие идемпотентно: повторный конец и закрытая человеком вкладка не закрывают дважды", async () => {
    const world = await startedEntry();
    await endOfSession(world, { kind: "returned_to_page" });
    await endOfSession(world, { kind: "returned_to_page" });
    assert.deepEqual(world.log.closed, [LINK_TAB]);
  });

  it("человек сейчас на ТРЕТЬЕЙ вкладке: исходную не активируем (он не прыгает), вкладка входа всё равно закрывается", async () => {
    const world = await startedEntry({ activeTab: 99 });
    await endOfSession(world, { kind: "returned_to_page" });
    assert.deepEqual(world.log.order, [`close ${LINK_TAB}`]);
  });

  it("исходной вкладки уже нет: возврат не вышел, но вкладка входа всё равно закрывается", async () => {
    const world = await startedEntry({ activateFails: true });
    await endOfSession(world, { kind: "returned_to_page" });
    assert.deepEqual(world.log.order, [`activate ${SOURCE.tabId}`, `close ${LINK_TAB}`]);
  });

  it("сбой tabs.remove отмечается для разработчика и наружу не выходит", async () => {
    const world = await startedEntry({ closeFails: true });
    await endOfSession(world, { kind: "returned_to_page" });
    assert.equal(world.notes.length, 1);
  });

  it("вкладку закрыл человек до конца сеанса: потом конец сеанса ничего не закрывает", async () => {
    const world = await startedEntry();
    world.entry.onTabRemoved(LINK_TAB);
    await endOfSession(world, { kind: "returned_to_page" });
    assert.deepEqual(world.log.order, []);
  });
});

// ------------------------------------------------------------------ часть 2: на настоящем владельце сеансов

const video = (element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate => ({ ...PLAYING_VIDEO, element, ...overrides });
const REF_B: VideoRef = { frame_id: 0, document: PLAYING_VIDEO.document, element: 2 };
const TWO_VIDEOS = [{ frameId: 0, result: [video(1), video(2, { playing: false, visible_area_px: 5000 })] }];

/** Вход по ссылке, чья вкладка — TAB настоящего владельца сеансов. */
function setupReal(frames = TWO_VIDEOS) {
  const world = setup(frames);
  const tabs: ReturnType<typeof fakeTabs> = fakeTabs({ activeTab: TAB });
  const { reporter, reports } = fakeReporter();
  const openedTabs: LinkTabsApi = { ...tabs.api, openActiveTab: (properties) => (tabs.log.opened.push(properties), Promise.resolve(TAB)) };
  const entry: LinkEntry = createLinkEntry({
    tabs: openedTabs,
    handoffs: {
      surveyTab: (tabId) => world.handoffs.surveyTab(tabId),
      handOffTab: (tabId) => world.handoffs.handOffTab(tabId),
      activeTabIds: () => world.handoffs.activeTabIds(),
    },
    scheduler: world.clock.scheduler,
    reporter,
    note: () => {},
  });
  world.handoffs.onSessionEnded((tabId, end) => entry.onSessionEnded(tabId, end));
  return { world, entry, tabs, reports };
}

/** Вход открыл вкладку, нашёл видео и довёл передачу до `started`. */
async function startThroughEntry(real: ReturnType<typeof setupReal>) {
  const done = real.entry.openLink("https://site.example/watch", SOURCE);
  await flushPromises();
  real.world.clock.advance(LINK_POLL_INTERVAL_MS);
  await flushPromises();
  const socket = real.world.sockets.last();
  socket.open();
  socket.receive({ type: "attached" });
  const port = real.world.framePorts.last();
  const begin = port.posted[0];
  assert.ok(begin?.kind === "begin_handoff");
  port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
  await flushPromises();
  await done;
  return { socket, port };
}

describe("вход по ссылке на настоящем владельце сеансов", () => {
  it("видео нашлось -> started -> плеер вернул видео на страницу (return_to_page): вкладка входа закрыта, человек на исходной", async () => {
    const real = setupReal();
    const { socket, port } = await startThroughEntry(real);
    assert.deepEqual(real.reports.map((report) => report.result), [{ kind: "outcome", outcome: { kind: "started", player_window: "new" } }]);
    assert.deepEqual(real.tabs.log.closed, [], "пока сеанс идёт, вкладка открыта");
    socket.receive({ type: "return_to_page", position_us: 5_000_000, resume: "paused" });
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(real.tabs.log.order, [`activate ${SOURCE.tabId}`, `close ${TAB}`]);
  });

  it("плеер закрыл окно (4000 player_closed): вкладка входа тоже закрыта", async () => {
    const real = setupReal();
    const { socket, port } = await startThroughEntry(real);
    socket.remoteClose(4000);
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(real.tabs.log.closed, [TAB]);
  });

  it("зритель нажал «Return» на заглушке: видео играет на вкладке входа, и она остаётся открытой", async () => {
    const real = setupReal();
    const { socket, port } = await startThroughEntry(real);
    port.deliver({ kind: "viewer_return_requested" });
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.equal(socket.closedWithCode, 4010);
    assert.deepEqual(real.world.ended.map((end) => end.kind), ["returned_by_viewer"]);
    assert.deepEqual(real.tabs.log.order, [], "ни активации исходной, ни закрытия");
    assert.deepEqual(real.entry.ownedTabIds(), [], "вкладка уже человека");
  });

  it("страница сама забрала видео (element_released): вкладка остаётся открытой", async () => {
    const real = setupReal();
    const { port } = await startThroughEntry(real);
    port.deliver({ kind: "handoff_ended", cause: "element_released" });
    await flushPromises();
    assert.deepEqual(real.world.ended.map((end) => end.kind), ["page_took_video_back"]);
    assert.deepEqual(real.tabs.log.order, []);
    assert.deepEqual(real.entry.ownedTabIds(), []);
  });

  it("замена видео на этой вкладке кончает старый сеанс, но вкладка не закрывается; закрывается концом нового", async () => {
    const real = setupReal();
    const { socket: firstSocket, port: firstPort } = await startThroughEntry(real);
    // Человек выбрал в попапе другое видео этой вкладки.
    const replacement = real.world.handoffs.handOffTab(TAB, undefined, REF_B);
    await flushPromises();
    const secondSocket = real.world.sockets.sockets[1];
    assert.ok(secondSocket !== undefined);
    secondSocket.open();
    secondSocket.receive({ type: "attached" });
    firstSocket.receive({ type: "return_to_page", position_us: 1, resume: "paused" });
    firstPort.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(real.world.ended.map((end) => end.kind), ["returned_to_page"], "старый сеанс кончился");
    assert.deepEqual(real.tabs.log.order, [], "но вкладку не трогаем: сеанс продолжается на другом видео");
    assert.deepEqual(real.entry.ownedTabIds(), [TAB]);

    const secondPort = real.world.framePorts.last();
    const begin = secondPort.posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    secondPort.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    assert.deepEqual(await replacement, { kind: "started", player_window: "new" });

    secondSocket.receive({ type: "return_to_page", position_us: 1, resume: "paused" });
    secondPort.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(real.tabs.log.closed, [TAB], "конец НОВОГО сеанса закрывает вкладку");
  });

  it("обычная вкладка (не открытая входом) с сеансом не закрывается никогда, как бы сеанс ни кончился", async () => {
    const real = setupReal();
    const handOff = real.world.handoffs.handOffTab(TAB);
    await flushPromises();
    real.world.sockets.last().open();
    real.world.sockets.last().receive({ type: "attached" });
    const port = real.world.framePorts.last();
    const begin = port.posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    await handOff;
    real.world.sockets.last().receive({ type: "return_to_page", position_us: 1, resume: "paused" });
    port.deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.deepEqual(real.world.ended.map((end) => end.kind), ["returned_to_page"]);
    assert.deepEqual(real.tabs.log.order, []);
  });
});

describe("TabHandoffs.onSessionEnded", () => {
  it("исключение одного слушателя не мешает остальным и журналу зависимостей", async () => {
    const world: World = setup();
    const heard: string[] = [];
    world.handoffs.onSessionEnded(() => {
      throw new Error("listener broke");
    });
    world.handoffs.onSessionEnded((_tabId, end) => void heard.push(end.kind));
    const handOff = world.handoffs.handOffTab(TAB);
    await flushPromises();
    world.sockets.last().open();
    world.sockets.last().receive({ type: "attached" });
    const port = world.framePorts.last();
    const begin = port.posted[0];
    assert.ok(begin?.kind === "begin_handoff");
    port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
    await handOff;
    port.deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(heard, ["page_took_video_back"]);
    assert.deepEqual(world.ended.map((end) => end.kind), ["page_took_video_back"]);
    assert.ok(world.adMirror.notes.some((note) => note.includes("session-ended listener threw")));
  });
});
