// Шаги попапа без DOM: что он спрашивает у фона, когда закрывается и что показывает после отказа.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { TabVideoRecord } from "../../src/background/tab-model/tab-snapshot.ts";
import type { HandOffTabOutcome, VideoRef } from "../../src/shared/handoff-contract.ts";
import type { SurveyTabReply, TabPlayerPresence } from "../../src/shared/survey-contract.ts";
import { createPopupFlow, type ActiveTab, type HandOffRequest, type PopupModel } from "../../src/popup/popup-flow.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";

function record(element: number, overrides: Partial<TabVideoRecord> = {}): TabVideoRecord {
  return {
    ...DEFAULT_CANDIDATE_FACTS,
    document: "doc-1",
    media: { kind: "media_source", in_worker: false },
    playing: false,
    visible_area_px: 1000,
    frame_id: 0,
    verdict: "main",
    element,
    ...overrides,
  };
}

const ref = (element: number): VideoRef => ({ frame_id: 0, document: "doc-1", element });

function surveyed(elements: readonly number[], inPlayer: TabPlayerPresence = { kind: "no_session" }): SurveyTabReply {
  return { kind: "surveyed", videos: elements.map((element) => record(element)), main: ref(elements[0] ?? 0), in_player: inPlayer };
}

/** Фейковый мир попапа: очереди ответов фона и журнал того, что попап сделал. */
function setupPopup(options: { tab?: ActiveTab | undefined; surveys?: SurveyTabReply[]; outcomes?: (HandOffTabOutcome | Error)[] }) {
  const surveys = [...(options.surveys ?? [surveyed([1, 2])])];
  const outcomes = [...(options.outcomes ?? [])];
  const calls: HandOffRequest[] = [];
  const surveyCalls: number[] = [];
  const models: PopupModel[] = [];
  let closed = 0;
  const flow = createPopupFlow({
    findActiveTab: () => Promise.resolve("tab" in options ? options.tab : { id: 7, title: "Movie night" }),
    surveyTab: (tabId) => {
      surveyCalls.push(tabId);
      const reply = surveys.length > 1 ? surveys.shift() : surveys[0];
      return reply === undefined ? Promise.reject(new Error("no reply")) : Promise.resolve(reply);
    },
    handOffTab: (request) => {
      calls.push(request);
      const next = outcomes.shift();
      if (next === undefined || next instanceof Error) return Promise.reject(next ?? new Error("no outcome"));
      return Promise.resolve(next);
    },
    closePopup: () => void (closed += 1),
    render: (model) => void models.push(model),
  });
  return { flow, calls, surveyCalls, models, closed: () => closed, last: () => models.at(-1) as PopupModel };
}

describe("попап: открытие", () => {
  it("видео вкладки уже в плеере: hand_off_tab не шлём, попап не закрываем, показываем «Playing» и список", async () => {
    const popup = setupPopup({ surveys: [surveyed([1, 2], { kind: "in_player", video: ref(1) })] });
    await popup.flow.start();
    assert.deepEqual(popup.calls, []);
    assert.equal(popup.closed(), 0);
    assert.equal(popup.last().notice.headline, "Playing in Fastiplayer");
    assert.deepEqual(popup.last().rows.map((row) => [row.ref.element, row.selectable, row.labels]), [[1, false, ["in player"]], [2, true, []]]);
    assert.deepEqual(popup.surveyCalls, [7], "опрошена активная вкладка");
  });

  it("сеанса нет: шлёт hand_off_tab главного видео вкладки, started закрывает попап", async () => {
    const popup = setupPopup({ outcomes: [{ kind: "started" }] });
    await popup.flow.start();
    assert.deepEqual(popup.calls, [{ tabId: 7 }]);
    assert.equal(popup.closed(), 1);
  });

  it("отказ не закрывает попап: причина текстом и список найденного", async () => {
    const popup = setupPopup({ outcomes: [{ kind: "no_video" }] });
    await popup.flow.start();
    assert.equal(popup.closed(), 0);
    assert.equal(popup.last().notice.id, "no_video");
    assert.match(popup.last().notice.details.join(" "), /Press play/);
    assert.deepEqual(popup.last().rows.map((row) => row.ref.element), [1, 2]);
    assert.equal(popup.last().busy, false);
  });

  it("страница браузера (unavailable): понятный текст, передачу не пробуем", async () => {
    const popup = setupPopup({ surveys: [{ kind: "unavailable", reason: "tab_not_scriptable" }] });
    await popup.flow.start();
    assert.deepEqual(popup.calls, []);
    assert.equal(popup.last().notice.id, "tab_not_scriptable");
    assert.deepEqual(popup.last().rows, []);
  });

  it("нет активной вкладки или фон не ответил на опрос: честный текст, без передачи, без падения", async () => {
    const noTab = setupPopup({ tab: undefined });
    await noTab.flow.start();
    assert.equal(noTab.last().notice.id, "no_active_tab");
    assert.deepEqual(noTab.calls, []);

    const silent = setupPopup({ surveys: [] });
    await silent.flow.start();
    assert.equal(silent.last().notice.id, "survey_failed");
    assert.deepEqual(silent.calls, []);
  });

  it("фон не ответил на hand_off_tab: исход failed, попап остаётся", async () => {
    const popup = setupPopup({ outcomes: [new Error("no answer")] });
    await popup.flow.start();
    assert.equal(popup.last().notice.id, "failed");
    assert.equal(popup.closed(), 0);
  });
});

describe("попап: выбор строки", () => {
  it("клик по строке шлёт hand_off_tab с этим видео; started закрывает попап (в том числе при замене)", async () => {
    const popup = setupPopup({ surveys: [surveyed([1, 2], { kind: "in_player", video: ref(1) })], outcomes: [{ kind: "started" }] });
    await popup.flow.start();
    await popup.flow.chooseVideo(ref(2));
    assert.deepEqual(popup.calls, [{ tabId: 7, video: ref(2) }]);
    assert.equal(popup.closed(), 1);
  });

  it("video_gone: причина текстом и список обновляется свежим опросом", async () => {
    const popup = setupPopup({
      surveys: [surveyed([1, 2]), surveyed([1])],
      outcomes: [{ kind: "no_video" }, { kind: "video_gone" }],
    });
    await popup.flow.start();
    assert.equal(popup.last().rows.length, 2);
    await popup.flow.chooseVideo(ref(2));
    assert.equal(popup.last().notice.id, "video_gone");
    assert.deepEqual(popup.last().rows.map((row) => row.ref.element), [1], "список обновлён");
    assert.equal(popup.closed(), 0);
  });

  it("пока команда идёт, повторные клики игнорируются", async () => {
    const popup = setupPopup({ outcomes: [{ kind: "no_video" }, { kind: "started" }] });
    await popup.flow.start();
    const first = popup.flow.chooseVideo(ref(2));
    const second = popup.flow.chooseVideo(ref(1));
    await Promise.all([first, second]);
    assert.deepEqual(popup.calls.map((call) => call.video?.element), [undefined, 2]);
  });
});

describe("попап: согласие на локальный адрес", () => {
  it("Allow повторяет команду с тем же video и согласием на хост", async () => {
    const popup = setupPopup({ outcomes: [{ kind: "no_video" }, { kind: "local_address_needs_consent", host: "nas.local" }, { kind: "started" }] });
    await popup.flow.start();
    await popup.flow.chooseVideo(ref(2));
    assert.equal(popup.last().question?.text, "This page wants the player to download from nas.local on your network. Allow?");
    assert.equal(popup.closed(), 0);
    await popup.flow.allowLocalAddress();
    assert.deepEqual(popup.calls.at(-1), { tabId: 7, video: ref(2), consentedLocalHost: "nas.local" });
    assert.equal(popup.closed(), 1);
  });

  it("вопрос на главное видео: повтор без video, но с согласием", async () => {
    const popup = setupPopup({ outcomes: [{ kind: "local_address_needs_consent", host: "nas.local" }, { kind: "started" }] });
    await popup.flow.start();
    await popup.flow.allowLocalAddress();
    assert.deepEqual(popup.calls, [{ tabId: 7 }, { tabId: 7, consentedLocalHost: "nas.local" }]);
  });

  it("Don't allow: вопрос уходит, повторной команды нет, видео остаётся в браузере", async () => {
    const popup = setupPopup({ outcomes: [{ kind: "local_address_needs_consent", host: "nas.local" }] });
    await popup.flow.start();
    popup.flow.declineLocalAddress();
    assert.equal(popup.last().question, null);
    assert.equal(popup.last().notice.id, "declined");
    assert.equal(popup.calls.length, 1);
    assert.equal(popup.closed(), 0);
    await popup.flow.allowLocalAddress();
    assert.equal(popup.calls.length, 1, "согласие после отказа ничего не шлёт");
  });
});
