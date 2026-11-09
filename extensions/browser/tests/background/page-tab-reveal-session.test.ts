// «Показать страницу» (протокол 1.6, решение 278) на уровне сеанса: команда плеера `show_page` переключает
// браузер на ТЕКУЩУЮ вкладку страницы. Фон целиком на фейковых часах, плеере, порте и браузере; браузерная
// сторона записывает вызовы по порядку, чтобы проверить порядок «вкладка -> фокус окна -> подсветка».
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { PageTabRevealApi } from "../../src/background/handoff/page-tab-reveal.ts";
import { translatePlayerMessage } from "../../src/background/handoff/player-message-translator.ts";
import { HELD_VIDEO_REPORT_KIND, type HeldVideoReport, type PageEvent, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { deliverStream, PLAYING_VIDEO, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { BROWSER_WINDOW, createFakePageReopenBrowser, REOPENED_TAB } from "./page-return/fake-page-reopen.ts";
import { flushPromises } from "./support.ts";

const TAB_URL = "https://www.example.com/watch?v=1";
/** Окно, в котором браузер нашёл вкладку: отличается от окна возврата, чтобы не спутать. */
const SHOWN_WINDOW = 77;

interface RevealFake {
  readonly api: PageTabRevealApi;
  /** Вызовы браузеру по порядку: `activate:<tab>`, `focus:<window>`, `attention:<window>`. */
  readonly calls: string[];
  /** Что делает `activateTab`: окно вкладки, пустота (вкладки нет) или отказ. */
  behavior: "window" | "gone" | "rejects";
  focusRejects: boolean;
}

function createRevealFake(): RevealFake {
  const fake: RevealFake = {
    calls: [],
    behavior: "window",
    focusRejects: false,
    api: {
      async activateTab(tabId) {
        fake.calls.push(`activate:${tabId}`);
        if (fake.behavior === "rejects") throw new Error("No tab with id");
        return fake.behavior === "gone" ? undefined : { windowId: SHOWN_WINDOW };
      },
      async focusWindow(windowId) {
        fake.calls.push(`focus:${windowId}`);
        if (fake.focusRejects) throw new Error("focus refused");
      },
      async drawAttention(windowId) {
        fake.calls.push(`attention:${windowId}`);
      },
    },
  };
  return fake;
}

async function activeSession(): Promise<{ world: World; reveal: RevealFake }> {
  const reveal = createRevealFake();
  const world = setup(undefined, undefined, undefined, { pageTabReveal: reveal.api });
  await startHandoff(world);
  deliverStream(world);
  return { world, reveal };
}

const developerNotes = (world: World): readonly string[] => world.adMirror.notes;

describe("show_page: вкладка сеанса вперёд", () => {
  it("активная передача: вкладка активна, окно просят вперёд, потом подсвечивают - именно в таком порядке", async () => {
    const { world, reveal } = await activeSession();
    world.sockets.last().receive({ type: "show_page" });
    await flushPromises();
    assert.deepEqual(reveal.calls, [`activate:${TAB}`, `focus:${SHOWN_WINDOW}`, `attention:${SHOWN_WINDOW}`]);
    assert.deepEqual(world.ended, [], "сеанс жив");
  });

  it("повторная просьба повторяет все три шага (зритель мог уйти в другое окно)", async () => {
    const { world, reveal } = await activeSession();
    world.sockets.last().receive({ type: "show_page" });
    await flushPromises();
    world.sockets.last().receive({ type: "show_page" });
    await flushPromises();
    assert.equal(reveal.calls.length, 6);
  });

  it("окно отказало в фокусе - подсветка всё равно идёт (это и есть запасной ход), отметка разработчику", async () => {
    const { world, reveal } = await activeSession();
    reveal.focusRejects = true;
    world.sockets.last().receive({ type: "show_page" });
    await flushPromises();
    assert.deepEqual(reveal.calls, [`activate:${TAB}`, `focus:${SHOWN_WINDOW}`, `attention:${SHOWN_WINDOW}`]);
    assert.ok(developerNotes(world).some((note) => note.includes("focusing the window")), developerNotes(world).join("|"));
  });

  it("вкладки уже нет (браузер вернул пустоту) - окон не трогаем, отметка разработчику, сеанс не упал", async () => {
    const { world, reveal } = await activeSession();
    reveal.behavior = "gone";
    world.sockets.last().receive({ type: "show_page" });
    await flushPromises();
    assert.deepEqual(reveal.calls, [`activate:${TAB}`]);
    assert.ok(developerNotes(world).some((note) => note.includes("is gone")));
    assert.deepEqual(world.ended, []);
  });

  it("браузер отверг активацию - исключения наружу нет, окон не трогаем, отметка разработчику", async () => {
    const { world, reveal } = await activeSession();
    reveal.behavior = "rejects";
    world.sockets.last().receive({ type: "show_page" });
    await flushPromises();
    assert.deepEqual(reveal.calls, [`activate:${TAB}`]);
    assert.ok(developerNotes(world).some((note) => note.includes("activating tab")));
    assert.deepEqual(world.ended, []);
  });
});

describe("show_page: чужие фазы и переезд страницы", () => {
  it("страница ушла сиротой (page_away): браузеру ничего, отметка разработчику", async () => {
    const reveal = createRevealFake();
    const reopen = createFakePageReopenBrowser({ url: TAB_URL, windowId: BROWSER_WINDOW, incognito: false });
    const world = setup(undefined, undefined, undefined, { tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser, pageTabReveal: reveal.api });
    await startHandoff(world);
    deliverStream(world);
    world.closeTab();
    world.framePorts.last().drop();
    world.handoffs.tabRemoved(TAB);
    await flushPromises();
    world.sockets.last().receive({ type: "show_page" });
    await flushPromises();
    assert.deepEqual(reveal.calls, []);
    assert.ok(developerNotes(world).some((note) => note.includes("the page is away")));
  });

  it("после возврата страницы в другую вкладку показывается НОВАЯ вкладка, а не прежняя", async () => {
    const reveal = createRevealFake();
    const reopen = createFakePageReopenBrowser({ url: TAB_URL, windowId: BROWSER_WINDOW, incognito: false });
    const world = setup(undefined, undefined, undefined, { tabWindowId: BROWSER_WINDOW, pageReopen: reopen.browser, pageTabReveal: reveal.api });
    await startHandoff(world);
    deliverStream(world);
    const socket = world.sockets.last();
    world.closeTab();
    world.framePorts.last().drop();
    world.handoffs.tabRemoved(TAB);
    await flushPromises();
    socket.receive({ type: "reopen_page", position_us: 61_000_000 });
    await flushPromises();
    const returnedFilm: VideoCandidate = { ...PLAYING_VIDEO, document: "doc-new", element: 9 };
    const report: HeldVideoReport = { kind: HELD_VIDEO_REPORT_KIND, element: returnedFilm.element, candidates: [returnedFilm], trigger: "next_video" };
    assert.equal(world.handoffs.heldVideo(REOPENED_TAB, 0, report), "adopt");
    const port = world.framePorts.last();
    const begin = port.posted.find((command) => command.kind === "begin_handoff");
    assert.ok(begin?.kind === "begin_handoff");
    port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" } satisfies PageEvent);
    await flushPromises();

    socket.receive({ type: "show_page" });
    await flushPromises();
    assert.deepEqual(reveal.calls, [`activate:${REOPENED_TAB}`, `focus:${SHOWN_WINDOW}`, `attention:${SHOWN_WINDOW}`]);
  });
});

describe("перевод show_page", () => {
  it("сообщение плеера становится действием show_page в любом поколении", () => {
    assert.deepEqual(translatePlayerMessage({ type: "show_page" }, undefined), { kind: "show_page" });
    assert.deepEqual(translatePlayerMessage({ type: "show_page" }, 3), { kind: "show_page" });
  });
});
