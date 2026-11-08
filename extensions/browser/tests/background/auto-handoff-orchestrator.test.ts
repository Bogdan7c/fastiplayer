// Режим 2 целиком на подделках браузера (12a): донесение сторожа -> решение фона -> передача вкладки /
// отпускание / кредит клика. Проверяем то, что видит страница и человек: ответ фону сторожа, команду передачи
// с точной ссылкой, просьбы фреймам, плашку.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createAutoHandoff, type AutoHandoff, type AutoHandoffDependencies } from "../../src/background/auto-handoff/auto-handoff.ts";
import { noticeOfAutoHandoffOutcome } from "../../src/background/auto-handoff/outcome-notice.ts";
import { PLAY_CREDIT_GRACE_MS } from "../../src/background/auto-handoff/viewer-play-decision.ts";
import type { EntryResult } from "../../src/background/entries/entry-report-text.ts";
import { createHeldVideoListener, type FrameSender } from "../../src/background/handoff/hand-off-command.ts";
import { DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import { HELD_VIDEO_REPORT_KIND, type HandOffTabOutcome, type HeldVideoReply, type HeldVideoReport, type HeldVideoTrigger, type VideoCandidate, type VideoRef } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { flushPromises } from "./support.ts";

const TAB = 12;
const FRAME = 3;
const FILM: VideoCandidate = { document: "doc-7f3a", element: 4, media: { kind: "media_source", in_worker: false }, playing: true, visible_area_px: 200_000, ...DEFAULT_CANDIDATE_FACTS };
const SHORT_CLIP: VideoCandidate = { ...FILM, media: { kind: "file" }, duration_s: 20, file_host: "site.example" };
const ON: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true };

const report = (candidate: VideoCandidate, trigger: HeldVideoTrigger = "viewer_play"): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element: candidate.element, candidates: [candidate], trigger });
const ref = (candidate: VideoCandidate): VideoRef => ({ frame_id: FRAME, document: candidate.document, element: candidate.element });

interface WorldOptions {
  readonly settings?: AutoHandoffSettings | "read_fails";
  readonly url?: string | undefined;
  readonly probeResult?: "ad_on_screen" | "no_ad";
  /** Исход передачи; можно отдать обещание, чтобы держать передачу «в пути». */
  readonly handOffOutcome?: Promise<HandOffTabOutcome>;
  readonly activeTabs?: number[];
  readonly heldVideoReply?: HeldVideoReply;
}

function createWorld(options: WorldOptions = {}) {
  const handOffCalls: { tabId: number; consent: unknown; video: VideoRef | undefined }[] = [];
  const heldVideoCalls: HeldVideoReport[] = [];
  const credits: { tabId: number; leaseMs: number }[] = [];
  const releases: { tabId: number; frameId: number; element: number }[] = [];
  const reports: { tabId: number; result: EntryResult }[] = [];
  const notes: string[] = [];
  const probes: number[] = [];
  const activeTabs = options.activeTabs ?? [];
  const dependencies: AutoHandoffDependencies = {
    handoffs: {
      heldVideo: (_tabId, _frameId, held) => {
        heldVideoCalls.push(held);
        return options.heldVideoReply ?? "release";
      },
      handOffTab: (tabId, consent, video) => {
        handOffCalls.push({ tabId, consent, video });
        return options.handOffOutcome ?? Promise.resolve({ kind: "started", player_window: "new" });
      },
      activeTabIds: () => activeTabs,
    },
    readSettings: () => (options.settings === "read_fails" ? Promise.reject(new Error("storage")) : Promise.resolve(options.settings ?? ON)),
    describeTab: async () => ("url" in options ? { url: options.url } : { url: "https://site.example/watch" }),
    adProbe: { probe: async (tabId) => (probes.push(tabId), options.probeResult ?? "no_ad") },
    grantPlayCredit: (tabId, leaseMs) => void credits.push({ tabId, leaseMs }),
    releaseHeldVideo: (tabId, frameId, element) => void releases.push({ tabId, frameId, element }),
    reporter: { report: async (tabId, result) => void reports.push({ tabId, result }) },
    note: (message) => void notes.push(message),
  };
  const autoHandoff: AutoHandoff = createAutoHandoff(dependencies);
  return { autoHandoff, handOffCalls, heldVideoCalls, credits, releases, reports, notes, probes, activeTabs };
}

describe("режим 2: передача по нажатию play", () => {
  it("принято: ответ adopt_new_session, передача вкладки с точной ссылкой на видео, плашки нет (новое окно)", async () => {
    const world = createWorld();
    const reply = await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    assert.equal(reply, "adopt_new_session");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(FILM) }]);
    await flushPromises();
    assert.deepEqual(world.releases, []);
    assert.deepEqual(world.reports, []);
  });

  it("режим выключен (настройка по умолчанию): release, передачи нет", async () => {
    const world = createWorld({ settings: DEFAULT_AUTO_HANDOFF_SETTINGS });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release");
    assert.deepEqual(world.handOffCalls, []);
  });

  it("сбой чтения настройки: режим считается выключенным, release, причина в журнале", async () => {
    const world = createWorld({ settings: "read_fails" });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release");
    assert.deepEqual(world.handOffCalls, []);
    assert.equal(world.notes.length, 1);
  });

  it("стоп-лист (встроенный и свой), вкладка без адреса и служебная страница: release без передачи", async () => {
    const cases: readonly { name: string; url: string | undefined; settings: AutoHandoffSettings }[] = [
      { name: "встроенный", url: "https://meet.google.com/abc", settings: ON },
      { name: "свой", url: "https://news.example/a", settings: { ...ON, own_stop_list: ["news.example"] } },
      { name: "без адреса", url: undefined, settings: ON },
      { name: "служебная страница", url: "chrome://settings", settings: ON },
    ];
    for (const row of cases) {
      const world = createWorld({ url: row.url, settings: row.settings });
      assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release", row.name);
      assert.deepEqual(world.handOffCalls, [], row.name);
    }
  });

  it("без звука и короче порога без рекламы: не передаём", async () => {
    const world = createWorld();
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report({ ...FILM, muted: true })), "release");
    assert.deepEqual(world.handOffCalls, []);
    assert.deepEqual(world.credits, []);
  });

  it("передача отказала (protected): фрейму уходит просьба отпустить именно это видео, человеку — тишина", async () => {
    const world = createWorld({ handOffOutcome: Promise.resolve({ kind: "protected" }) });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "adopt_new_session");
    await flushPromises();
    assert.deepEqual(world.releases, [{ tabId: TAB, frameId: FRAME, element: FILM.element }]);
    assert.deepEqual(world.reports, []);
  });

  it("плеер недоступен: просьба отпустить и плашка отказа", async () => {
    const outcome: HandOffTabOutcome = { kind: "player_unavailable", reason: "player_not_responding" };
    const world = createWorld({ handOffOutcome: Promise.resolve(outcome) });
    await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    await flushPromises();
    assert.equal(world.releases.length, 1);
    assert.deepEqual(world.reports, [{ tabId: TAB, result: { kind: "outcome", outcome } }]);
  });

  it("окно плеера уже открыто: видео не отпускается, человеку плашка «переключись на окно»", async () => {
    const outcome: HandOffTabOutcome = { kind: "started", player_window: "already_open" };
    const world = createWorld({ handOffOutcome: Promise.resolve(outcome) });
    await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    await flushPromises();
    assert.deepEqual(world.releases, []);
    assert.deepEqual(world.reports, [{ tabId: TAB, result: { kind: "outcome", outcome } }]);
  });

  it("короткий ролик без рекламы: кредит клика всем фреймам на длину + запас, ответ release, передачи нет", async () => {
    const world = createWorld({ probeResult: "no_ad" });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(SHORT_CLIP)), "release");
    assert.deepEqual(world.probes, [TAB]);
    assert.deepEqual(world.credits, [{ tabId: TAB, leaseMs: 20_000 + PLAY_CREDIT_GRACE_MS }]);
    assert.deepEqual(world.handOffCalls, []);
  });

  it("короткий ролик, на вкладке видна реклама: передаём, кредита нет", async () => {
    const world = createWorld({ probeResult: "ad_on_screen" });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(SHORT_CLIP)), "adopt_new_session");
    assert.deepEqual(world.handOffCalls.map((call) => call.video), [ref(SHORT_CLIP)]);
    assert.deepEqual(world.credits, []);
  });

  it("известная реклама (рекламный хост) передаётся без пробы", async () => {
    const world = createWorld();
    const ad = { ...SHORT_CLIP, file_host: "doubleclick.net" };
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(ad)), "adopt_new_session");
    assert.deepEqual(world.probes, []);
  });
});

describe("режим 2: маршрутизация и одно решение на вкладку", () => {
  it("next_video идёт в передачу вкладки без изменений и синхронно", () => {
    const world = createWorld({ heldVideoReply: "adopt" });
    const held = report(FILM, "next_video");
    assert.equal(world.autoHandoff.heldVideo(TAB, FRAME, held), "adopt");
    assert.deepEqual(world.heldVideoCalls, [held]);
    assert.deepEqual(world.handOffCalls, []);
  });

  it("у вкладки живая передача и её сторож взял видео (переезд 09c2): adopt, новой передачи нет", () => {
    const world = createWorld({ activeTabs: [TAB], heldVideoReply: "adopt" });
    assert.equal(world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "adopt");
    assert.equal(world.heldVideoCalls.length, 1);
    assert.deepEqual(world.handOffCalls, []);
  });

  it("у вкладки живая передача, а человек запустил другое видео: оно заменяет прежнее через handOffTab с выбранным видео", async () => {
    const world = createWorld({ activeTabs: [TAB], heldVideoReply: "release" });
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "adopt_new_session");
    assert.equal(world.heldVideoCalls.length, 1, "сначала спросили сеанс вкладки");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(FILM) }]);
  });

  it("второе донесение, пока первое решается или стартует: release; после исхода вкладка свободна", async () => {
    let finish: (outcome: HandOffTabOutcome) => void = () => {};
    const world = createWorld({ handOffOutcome: new Promise((resolve) => (finish = resolve)) });
    const first = world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    assert.equal(world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release", "пока решается");
    assert.equal(await first, "adopt_new_session");
    assert.equal(world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "release", "пока стартует");
    assert.equal(world.handOffCalls.length, 1);
    finish({ kind: "protected" });
    await flushPromises();
    assert.equal(await world.autoHandoff.heldVideo(TAB, FRAME, report(FILM)), "adopt_new_session", "после исхода");
    assert.equal(world.handOffCalls.length, 2);
  });

  it("донесения разных вкладок не мешают друг другу", async () => {
    const world = createWorld({ handOffOutcome: new Promise(() => {}) });
    assert.equal(await world.autoHandoff.heldVideo(1, FRAME, report(FILM)), "adopt_new_session");
    assert.equal(await world.autoHandoff.heldVideo(2, FRAME, report(FILM)), "adopt_new_session");
    assert.deepEqual(world.handOffCalls.map((call) => call.tabId), [1, 2]);
  });

  it("другой вход начал передачу этой вкладки, пока мы судили: решает handOffTab (то же видео или замена)", async () => {
    const world = createWorld();
    const answer = world.autoHandoff.heldVideo(TAB, FRAME, report(FILM));
    world.activeTabs.push(TAB);
    assert.equal(await answer, "adopt_new_session");
    assert.deepEqual(world.handOffCalls, [{ tabId: TAB, consent: undefined, video: ref(FILM) }]);
  });
});

describe("слушатель донесений: ответ позже", () => {
  const sender: FrameSender = { id: "ext", tab: { id: TAB }, frameId: FRAME };

  it("готовый ответ уходит сразу и канал не держится; обещание держит канал и отвечает позже", async () => {
    const world = createWorld();
    const replies: HeldVideoReply[] = [];
    const listener = createHeldVideoListener("ext", (tabId, frameId, held) => world.autoHandoff.heldVideo(tabId, frameId, held));
    assert.equal(listener(report(FILM, "next_video"), sender, (reply) => replies.push(reply)), false);
    assert.deepEqual(replies, ["release"]);
    assert.equal(listener(report(FILM), sender, (reply) => replies.push(reply)), true);
    assert.deepEqual(replies, ["release"]);
    await flushPromises();
    assert.deepEqual(replies, ["release", "adopt_new_session"]);
  });

  it("сорвавшееся обещание — release и отметка в журнале", async () => {
    const notes: string[] = [];
    const replies: HeldVideoReply[] = [];
    const listener = createHeldVideoListener("ext", () => Promise.reject(new Error("boom")), (message) => notes.push(message));
    assert.equal(listener(report(FILM), sender, (reply) => replies.push(reply)), true);
    await flushPromises();
    assert.deepEqual(replies, ["release"]);
    assert.equal(notes.length, 1);
  });
});

describe("что сказать человеку об исходе", () => {
  it("таблица: молчим, кроме окна «уже открыто» и отказов плеера", () => {
    const outcomes: readonly { outcome: HandOffTabOutcome; speaks: boolean }[] = [
      { outcome: { kind: "started", player_window: "new" }, speaks: false },
      { outcome: { kind: "started", player_window: "already_open" }, speaks: true },
      { outcome: { kind: "player_unavailable", reason: "x" }, speaks: true },
      { outcome: { kind: "failed", reason: "x" }, speaks: true },
      { outcome: { kind: "no_video" }, speaks: false },
      { outcome: { kind: "protected" }, speaks: false },
      { outcome: { kind: "not_supported_yet", form: "native_hls" }, speaks: false },
      { outcome: { kind: "reload_needed", reason: "script_missing" }, speaks: false },
      { outcome: { kind: "video_gone" }, speaks: false },
      { outcome: { kind: "local_address_refused" }, speaks: false },
      { outcome: { kind: "local_address_needs_consent", host: "nas.local" }, speaks: false },
    ];
    for (const row of outcomes) {
      const notice = noticeOfAutoHandoffOutcome(row.outcome);
      assert.deepEqual(notice, row.speaks ? { kind: "outcome", outcome: row.outcome } : undefined, row.outcome.kind);
    }
  });
});
