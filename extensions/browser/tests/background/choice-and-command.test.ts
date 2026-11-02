// Выбор видео (пункт 9 09a), опрос фреймов и приём команды «отдать вкладку N» (решение 175).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { describeFrameVideos, surveyTabVideos, type ExecuteInAllFrames } from "../../src/background/handoff/frame-survey.ts";
import { createHandOffCommandListener, isOwnExtensionPage } from "../../src/background/handoff/hand-off-command.ts";
import { pageIdentityOf, siteDomainOf, UNKNOWN_SITE_DOMAIN } from "../../src/background/handoff/page-identity.ts";
import { translatePlayerMessage } from "../../src/background/handoff/player-message-translator.ts";
import { chooseVideo } from "../../src/background/handoff/video-choice.ts";
import type { HandOffTabOutcome, VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { VIDEO_CANDIDATES_FUNCTION_NAME } from "../../src/shared/video-candidates-probe.ts";
import { flushPromises } from "./support.ts";

function video(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return { document: "doc-top", element, has_live_media: true, playing: false, visible_area_px: 1000, ...overrides };
}

describe("выбор видео", () => {
  it("играющий элемент с живым MSE побеждает больший неиграющий, из любого фрейма", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { visible_area_px: 900_000 })] },
      { frameId: 7, candidates: [video(2, { document: "doc-iframe", playing: true, visible_area_px: 50_000 })] },
    ]);
    assert.deepEqual(choice, { frameId: 7, document: "doc-iframe", element: 2 }, "метка документа выбранного элемента запомнена");
  });

  it("никто не играет — самый большой видимый живой", () => {
    const choice = chooseVideo([
      { frameId: 0, candidates: [video(1, { visible_area_px: 100 }), video(2, { visible_area_px: 5000 })] },
      { frameId: 4, candidates: [video(3, { visible_area_px: 4000 })] },
    ]);
    assert.deepEqual(choice, { frameId: 0, document: "doc-top", element: 2 });
  });

  it("играющее видео без MSE (`<video src>`) в 09a не выбирается", () => {
    const choice = chooseVideo([{ frameId: 0, candidates: [video(1, { has_live_media: false, playing: true }), video(2)] }]);
    assert.deepEqual(choice, { frameId: 0, document: "doc-top", element: 2 });
  });

  it("живые, но не играют и не видны — передавать нечего", () => {
    assert.equal(chooseVideo([{ frameId: 0, candidates: [video(1, { visible_area_px: 0 })] }]), undefined);
    assert.equal(chooseVideo([]), undefined);
  });

  it("при равенстве — верхний фрейм и порядок документа, независимо от порядка ответов", () => {
    const choice = chooseVideo([
      { frameId: 9, candidates: [video(5, { playing: true })] },
      { frameId: 0, candidates: [video(1, { playing: true }), video(2, { playing: true })] },
    ]);
    assert.deepEqual(choice, { frameId: 0, document: "doc-top", element: 1 });
  });
});

describe("опрос фреймов", () => {
  it("внедряемая функция зовёт функцию изолированного глобала и возвращает её результат", async () => {
    const candidates = [video(1)];
    const global = globalThis as Record<string, unknown>;
    global[VIDEO_CANDIDATES_FUNCTION_NAME] = () => Promise.resolve(candidates);
    try {
      assert.deepEqual(await describeFrameVideos(VIDEO_CANDIDATES_FUNCTION_NAME), candidates);
    } finally {
      delete global[VIDEO_CANDIDATES_FUNCTION_NAME];
    }
    assert.equal(describeFrameVideos(VIDEO_CANDIDATES_FUNCTION_NAME), null, "фрейм без нашего скрипта — null");
  });

  it("внедряемая функция самодостаточна: её текст работает без замыканий модуля", () => {
    const rebuilt = new Function(`return (${describeFrameVideos.toString()})`)() as (name: string) => unknown;
    const global = globalThis as Record<string, unknown>;
    global["probeName"] = () => "ok";
    try {
      assert.equal(rebuilt("probeName"), "ok");
    } finally {
      delete global["probeName"];
    }
  });

  it("собирает договорные ответы фреймов, кривые и пустые отбрасывает", async () => {
    const execute: ExecuteInAllFrames = (tabId, func, args) => {
      assert.equal(tabId, 5);
      assert.equal(func, describeFrameVideos);
      assert.deepEqual(args, [VIDEO_CANDIDATES_FUNCTION_NAME]);
      return Promise.resolve([
        { frameId: 0, result: [video(1)] },
        { frameId: 1, result: null },
        { frameId: 2, result: [{ ...video(2), extra: true }] },
        { frameId: 3, result: [] },
        { frameId: 4 },
      ]);
    };
    assert.deepEqual(await surveyTabVideos(5, execute), [{ frameId: 0, candidates: [video(1)] }]);
  });
});

describe("команда «отдать вкладку N»", () => {
  const identity = { extensionId: "ourid", extensionBaseUrl: "chrome-extension://ourid/" };

  it("страница экстеншена — своя; контент-скрипт, чужой экстеншен и подделка адреса — нет", () => {
    assert.equal(isOwnExtensionPage({ id: "ourid", url: "chrome-extension://ourid/popup.html" }, identity), true);
    assert.equal(isOwnExtensionPage({ id: "ourid", url: "https://evil.example/chrome-extension://ourid/" }, identity), false);
    assert.equal(isOwnExtensionPage({ id: "other", url: "chrome-extension://ourid/popup.html" }, identity), false);
    assert.equal(isOwnExtensionPage({ id: "ourid", url: "chrome-extension://other/popup.html" }, identity), false);
    assert.equal(isOwnExtensionPage({ id: "ourid" }, identity), false);
    assert.equal(isOwnExtensionPage({ id: "ourid", url: "chrome-extension://ourid/x" }, { ...identity, extensionBaseUrl: "" }), false);
  });

  it("от своей страницы команда исполняется и исход уходит в ответ", async () => {
    const asked: number[] = [];
    const listener = createHandOffCommandListener(identity, (tabId) => {
      asked.push(tabId);
      return Promise.resolve({ kind: "started" });
    });
    const responses: HandOffTabOutcome[] = [];
    const keepsChannel = listener({ kind: "hand_off_tab", tab_id: 31 }, { id: "ourid", url: "chrome-extension://ourid/bench.html" }, (response) =>
      responses.push(response),
    );
    assert.equal(keepsChannel, true);
    await flushPromises();
    assert.deepEqual(asked, [31]);
    assert.deepEqual(responses, [{ kind: "started" }]);
  });

  it("от контент-скрипта (адрес сайта) и кривые команды не исполняются", () => {
    const asked: number[] = [];
    const listener = createHandOffCommandListener(identity, (tabId) => {
      asked.push(tabId);
      return Promise.resolve({ kind: "started" });
    });
    const ignore = () => assert.fail("ответа быть не должно");
    assert.equal(listener({ kind: "hand_off_tab", tab_id: 1 }, { id: "ourid", url: "https://www.youtube.com/watch" }, ignore), false);
    for (const malformed of [
      { kind: "hand_off_tab" },
      { kind: "hand_off_tab", tab_id: -1 },
      { kind: "hand_off_tab", tab_id: 1.5 },
      { kind: "hand_off_tab", tab_id: "1" },
      { kind: "hand_off_tab", tab_id: 1, extra: 0 },
      { kind: "other", tab_id: 1 },
      null,
    ]) {
      assert.equal(listener(malformed, { id: "ourid", url: "chrome-extension://ourid/popup.html" }, ignore), false);
    }
    assert.deepEqual(asked, []);
  });
});

describe("страница для open_session и сообщения плеера", () => {
  it("заголовок чистится, домен — только допустимый протоколом", () => {
    assert.deepEqual(pageIdentityOf({ title: "A‮B\nC", url: "https://Пример.укр/x?y" }), { title: "AB C", domain: "xn--e1afmkfd.xn--j1amh" });
    assert.equal(siteDomainOf("file:///home/video.html"), UNKNOWN_SITE_DOMAIN);
    assert.equal(siteDomainOf("not a url"), UNKNOWN_SITE_DOMAIN);
    assert.equal(siteDomainOf("http://[::1]:8080/"), "[::1]");
  });

  it("часы плеера теряют seq, return_to_page даёт точный возврат, fetch_range в 09a пропускается", () => {
    assert.deepEqual(translatePlayerMessage({ type: "clock", seq: 3, position_us: 10, state: "waiting", rate: 500 }, 1), {
      kind: "shadow",
      command: { kind: "clock", position_us: 10, state: "waiting", rate: 500 },
    });
    assert.deepEqual(translatePlayerMessage({ type: "return_to_page", position_us: 7, resume: "paused" }, 1), {
      kind: "end_handoff",
      position: { kind: "at", position_us: 7 },
      resume: { kind: "exactly", resume: "paused" },
      cause: "returned_to_page",
    });
    assert.deepEqual(translatePlayerMessage({ type: "fetch_range", request_id: 1, offset: 0, length: 10 }, 1), { kind: "ignore" });
    assert.deepEqual(translatePlayerMessage({ type: "stream_rejected", generation: 1, reason: "unsupported_codec" }, undefined), { kind: "ignore" });
  });
});
