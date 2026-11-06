// Опрос вкладки без передачи (survey_tab) и передача ВЫБРАННОГО видео (hand_off_tab с `video`), сессия 10b:
// плеер не будится опросом, выбранное видео уходит именно оно, устаревшая ссылка — честный отказ без подмены.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { SURVEY_TIMEOUT_MS } from "../../src/background/handoff/frame-survey.ts";
import { createHandOffCommandListener, parseHandOffTabRequest, type CommandSender } from "../../src/background/handoff/hand-off-command.ts";
import { createSurveyTabCommandListener, parseSurveyTabRequest } from "../../src/background/handoff/survey-command.ts";
import { chooseRequestedVideo } from "../../src/background/handoff/video-choice.ts";
import type { HandOffTabOutcome, VideoCandidate, VideoRef } from "../../src/shared/handoff-contract.ts";
import type { SurveyTabReply } from "../../src/shared/survey-contract.ts";
import { PLAYING_VIDEO, setup, TAB } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

function video(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return { ...PLAYING_VIDEO, element, ...overrides };
}

const refOf = (element: number, document = PLAYING_VIDEO.document, frameId = 0): VideoRef => ({ frame_id: frameId, document, element });

/** Главное видео (играет) и второе, побольше мелкое, не играет. */
const TWO_VIDEOS = [{ frameId: 0, result: [video(1), video(2, { playing: false, visible_area_px: 5000 })] }];

const IDENTITY = { extensionId: "ext-id", extensionBaseUrl: "chrome-extension://ext-id/" };
const POPUP: CommandSender = { id: "ext-id", url: "chrome-extension://ext-id/popup.html" };
/** Контент-скрипт нашего же экстеншена: свой id, но адрес страницы сайта. */
const CONTENT_SCRIPT: CommandSender = { id: "ext-id", url: "https://evil.example/page" };

/** Довести передачу выбранного видео до «страница ответила `started`» и вернуть команду `begin_handoff`. */
async function completeStart(world: ReturnType<typeof setup>, outcome: Promise<HandOffTabOutcome>) {
  await flushPromises();
  world.sockets.last().open();
  world.sockets.last().receive({ type: "attached" });
  const port = world.framePorts.last();
  const begin = port.posted[0];
  assert.ok(begin?.kind === "begin_handoff");
  port.deliver({ kind: "handoff_began", request_id: begin.request_id, outcome: "started" });
  assert.deepEqual(await outcome, { kind: "started" });
  return begin;
}

describe("survey_tab: опрос без передачи", () => {
  it("не будит плеер, пишет модель вкладки и отвечает списком, главным видео и «сеанса нет»", async () => {
    const world = setup(TWO_VIDEOS);
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.main, refOf(1));
    assert.deepEqual(reply.videos.map((record) => record.element), [1, 2], "главное первым");
    assert.deepEqual(reply.in_player, { kind: "no_session" });
    // Плеер не разбужен: ни запроса сеанса, ни нужды связи, ни сокета, ни порта к странице.
    assert.equal(world.door.requests.length, 0);
    assert.equal(world.door.demandAcquired(), 0);
    assert.equal(world.sockets.sockets.length, 0);
    assert.equal(world.framePorts.ports.length, 0);
    // Модель вкладки обновлена тем же опросом.
    assert.deepEqual(await world.tabModel.mainVideoOf(TAB), refOf(1));
    assert.equal((await world.tabModel.videosFoundIn(TAB)).length, 2);
  });

  it("свежий опрос сбрасывает прежний отказ в модели", async () => {
    const world = setup([{ frameId: 0, result: null }]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB), { kind: "no_video" });
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "no_video" });
    await world.handoffs.surveyTab(TAB);
    assert.equal(await world.tabModel.lastRefusalOf(TAB), null);
  });

  it("сеанс есть только после started: пока передача стартует — «сеанса нет», потом видео в плеере", async () => {
    const world = setup(TWO_VIDEOS);
    const outcome = world.handoffs.handOffTab(TAB);
    await flushPromises();
    const during = await world.handoffs.surveyTab(TAB);
    assert.ok(during.kind === "surveyed");
    assert.deepEqual(during.in_player, { kind: "no_session" }, "передача ещё стартует");
    await completeStart(world, outcome);
    const after = await world.handoffs.surveyTab(TAB);
    assert.ok(after.kind === "surveyed");
    assert.deepEqual(after.in_player, { kind: "in_player", video: refOf(1) });
  });

  it("служебная страница браузера — unavailable{tab_not_scriptable}, модель не трогаем", async () => {
    const world = setup();
    world.forbidSurveys(new Error("Cannot access a chrome:// URL"));
    assert.deepEqual(await world.handoffs.surveyTab(TAB), { kind: "unavailable", reason: "tab_not_scriptable" });
    assert.equal(world.door.requests.length, 0);
    assert.deepEqual(await world.tabModel.videosFoundIn(TAB), []);
  });

  it("фреймы не ответили вовремя — unavailable{survey_timed_out}", async () => {
    const world = setup(new Promise(() => {}));
    const reply = world.handoffs.surveyTab(TAB);
    world.clock.advance(SURVEY_TIMEOUT_MS);
    assert.deepEqual(await reply, { kind: "unavailable", reason: "survey_timed_out" });
  });
});

describe("команды попапа: кто и в какой форме может их слать", () => {
  const SURVEY_REPLY: SurveyTabReply = { kind: "unavailable", reason: "tab_not_scriptable" };

  function surveyListener() {
    const asked: number[] = [];
    const listener = createSurveyTabCommandListener(IDENTITY, (tabId) => {
      asked.push(tabId);
      return Promise.resolve(SURVEY_REPLY);
    });
    return { listener, asked };
  }

  function handOffListener() {
    const asked: { tabId: number; video: VideoRef | undefined; consent: string }[] = [];
    const listener = createHandOffCommandListener(IDENTITY, (tabId, consent, requested) => {
      asked.push({ tabId, video: requested, consent: consent.kind === "given_for" ? consent.host : "none" });
      return Promise.resolve<HandOffTabOutcome>({ kind: "started" });
    });
    return { listener, asked };
  }

  it("страница экстеншена получает ответ на обе команды", async () => {
    const survey = surveyListener();
    const replies: unknown[] = [];
    assert.equal(survey.listener({ kind: "survey_tab", tab_id: 5 }, POPUP, (reply) => void replies.push(reply)), true);
    await flushPromises();
    assert.deepEqual(replies, [SURVEY_REPLY]);
    assert.deepEqual(survey.asked, [5]);

    const handOff = handOffListener();
    const outcomes: unknown[] = [];
    const message = { kind: "hand_off_tab", tab_id: 5, video: refOf(2, "doc-b", 3), consented_local_host: "nas.local" };
    assert.equal(handOff.listener(message, POPUP, (outcome) => void outcomes.push(outcome)), true);
    await flushPromises();
    assert.deepEqual(outcomes, [{ kind: "started" }]);
    assert.deepEqual(handOff.asked, [{ tabId: 5, video: refOf(2, "doc-b", 3), consent: "nas.local" }]);
  });

  it("контент-скрипт и чужой отправитель не получают ответа ни на одну команду, команда не доходит", async () => {
    const survey = surveyListener();
    const handOff = handOffListener();
    for (const sender of [CONTENT_SCRIPT, { id: "other-ext", url: "chrome-extension://ext-id/popup.html" }, {}]) {
      assert.equal(survey.listener({ kind: "survey_tab", tab_id: 5 }, sender, () => assert.fail("ответ чужому")), false);
      assert.equal(handOff.listener({ kind: "hand_off_tab", tab_id: 5, video: refOf(2) }, sender, () => assert.fail("ответ чужому")), false);
    }
    await flushPromises();
    assert.deepEqual(survey.asked, []);
    assert.deepEqual(handOff.asked, []);
  });

  it("строгий разбор survey_tab: лишнее поле, кривой номер вкладки, чужой вид — не наша команда", () => {
    assert.deepEqual(parseSurveyTabRequest({ kind: "survey_tab", tab_id: 0 }), { kind: "survey_tab", tab_id: 0 });
    for (const bad of [
      { kind: "survey_tab", tab_id: 5, extra: 1 },
      { kind: "survey_tab", tab_id: -1 },
      { kind: "survey_tab", tab_id: 1.5 },
      { kind: "survey_tab", tab_id: "5" },
      { kind: "survey_tab" },
      { kind: "hand_off_tab", tab_id: 5 },
      null,
      [],
    ]) {
      assert.equal(parseSurveyTabRequest(bad), undefined, JSON.stringify(bad));
    }
  });

  it("строгий разбор hand_off_tab с video: лишнее поле, кривая ссылка, неполная ссылка — не наша команда", () => {
    assert.deepEqual(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 5, video: refOf(2, "doc-b", 3) }), {
      kind: "hand_off_tab",
      tab_id: 5,
      video: refOf(2, "doc-b", 3),
    });
    assert.deepEqual(parseHandOffTabRequest({ kind: "hand_off_tab", tab_id: 5 }), { kind: "hand_off_tab", tab_id: 5 }, "без video — как раньше");
    const good = refOf(2, "doc-b", 3);
    for (const bad of [
      { kind: "hand_off_tab", tab_id: 5, video: { ...good, extra: 1 } },
      { kind: "hand_off_tab", tab_id: 5, video: { frame_id: 0, document: "doc-b" } },
      { kind: "hand_off_tab", tab_id: 5, video: { ...good, frame_id: "0" } },
      { kind: "hand_off_tab", tab_id: 5, video: { ...good, frame_id: -1 } },
      { kind: "hand_off_tab", tab_id: 5, video: { ...good, element: 1.5 } },
      { kind: "hand_off_tab", tab_id: 5, video: { ...good, document: "no spaces allowed" } },
      { kind: "hand_off_tab", tab_id: 5, video: { ...good, document: "" } },
      { kind: "hand_off_tab", tab_id: 5, video: null },
      { kind: "hand_off_tab", tab_id: 5, video: [] },
      { kind: "hand_off_tab", tab_id: 5, video: good, extra: 1 },
    ]) {
      assert.equal(parseHandOffTabRequest(bad), undefined, JSON.stringify(bad));
    }
  });
});

describe("hand_off_tab с выбранным видео", () => {
  it("уходит именно выбранное, а не главное: begin_handoff несёт его элемент", async () => {
    const world = setup(TWO_VIDEOS);
    const outcome = world.handoffs.handOffTab(TAB, undefined, refOf(2));
    const begin = await completeStart(world, outcome);
    assert.deepEqual(begin, { kind: "begin_handoff", request_id: begin.request_id, document: PLAYING_VIDEO.document, element: 2 });
    const reply = await world.handoffs.surveyTab(TAB);
    assert.ok(reply.kind === "surveyed");
    assert.deepEqual(reply.in_player, { kind: "in_player", video: refOf(2) });
  });

  it("устаревшая ссылка (элемента нет или документ другой) — video_gone, плеер не разбужен, подмены нет", async () => {
    const world = setup(TWO_VIDEOS);
    for (const stale of [refOf(99), refOf(2, "doc-old"), refOf(2, PLAYING_VIDEO.document, 7)]) {
      assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, stale), { kind: "video_gone" });
    }
    assert.equal(world.door.requests.length, 0, "главное видео вместо пропавшего не отдаётся");
    assert.equal(world.door.demandAcquired(), 0);
    assert.equal(world.framePorts.ports.length, 0);
    assert.deepEqual(world.handoffs.activeTabIds(), []);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "video_gone" });
  });

  it("DRM, неподдержанная форма и пустой элемент по ссылке — свои отказы, а главное видео не подменяет", async () => {
    const world = setup([
      {
        frameId: 0,
        result: [
          video(1),
          video(2, { drm: true }),
          video(3, { media: { kind: "unsupported", form: "native_hls" }, playing: true }),
          video(4, { media: { kind: "none" }, playing: false }),
        ],
      },
    ]);
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, refOf(2)), { kind: "protected" });
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, refOf(3)), { kind: "not_supported_yet", form: "native_hls" });
    assert.deepEqual(await world.handoffs.handOffTab(TAB, undefined, refOf(4)), { kind: "no_video" });
    assert.equal(world.door.requests.length, 0);
    assert.deepEqual(await world.tabModel.lastRefusalOf(TAB), { kind: "no_video" });
  });

  it("реклама и «мусор» по выбору человека передаются, и фильм, принятый за рекламу, не получает роль рекламы", async () => {
    const ad = video(2, { media: { kind: "file" }, file_host: "securepubads.g.doubleclick.net", duration_s: 15, playing: false, visible_area_px: 5000 });
    const world = setup([{ frameId: 0, result: [video(1), ad] }]);
    const outcome = world.handoffs.handOffTab(TAB, undefined, refOf(2));
    const begin = await completeStart(world, outcome);
    assert.equal(begin.element, 2);
    assert.equal(world.handoffs.isAdOnScreen(TAB), false, "на экране плеера не реклама: человек выбрал это видео");
  });

  it("чистый выбор: роль всегда film, mayBeAd считается как обычно, неизвестная ссылка — gone", () => {
    const shortAd = video(2, { media: { kind: "file" }, file_host: "x.doubleclick.net", duration_s: 15 });
    const frames = [{ frameId: 0, candidates: [video(1), shortAd] }];
    const choice = chooseRequestedVideo(frames, refOf(2));
    assert.ok(choice.kind === "hand_off");
    assert.equal(choice.video.role, "film");
    assert.equal(choice.video.mayBeAd, true, "короткий файл всё равно может оказаться роликом");
    assert.deepEqual(chooseRequestedVideo(frames, refOf(3)), { kind: "gone" });
  });
});
