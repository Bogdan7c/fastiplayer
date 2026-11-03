// Переопределения ответов про кодеки: отказ/не отказ, вызов оригинала, чужие обёртки, мусор от страницы.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { installCodecAnswerOverrides } from "../../src/page/codec-policy/answer-overrides.ts";
import { createPolicyCell, type PolicyCell } from "../../src/page/codec-policy/policy-cell.ts";
import { refuse, type CodecPolicy } from "../../src/shared/codec-policy.ts";

const AV1 = 'video/mp4; codecs="av01.0.05M.08"';
const H264 = 'video/mp4; codecs="avc1.64001f, mp4a.40.2"';
const HLS = "application/vnd.apple.mpegurl";

interface Call {
  readonly self: unknown;
  readonly args: readonly unknown[];
}

/** Браузер в миниатюре: всё «поддерживается», вызовы записываются. */
function createFakeBrowser() {
  const calls = { isTypeSupported: [] as Call[], canPlayType: [] as Call[], decodingInfo: [] as Call[] };
  class FakeMediaSource {
    static isTypeSupported(this: unknown, type: string): boolean {
      calls.isTypeSupported.push({ self: this, args: [type] });
      if (arguments.length === 0) throw new TypeError("1 argument required");
      return true;
    }
  }
  class FakeHTMLMediaElement {
    canPlayType(this: unknown, type: string): string {
      calls.canPlayType.push({ self: this, args: [type] });
      if (!(this instanceof FakeHTMLMediaElement)) throw new TypeError("Illegal invocation");
      return "probably";
    }
  }
  class FakeMediaCapabilities {
    decodingInfo(this: unknown, configuration: unknown): Promise<object> {
      calls.decodingInfo.push({ self: this, args: [configuration] });
      if (typeof configuration !== "object" || configuration === null) return Promise.reject(new TypeError("bad configuration"));
      return Promise.resolve({ supported: true, smooth: true, powerEfficient: true, keySystemAccess: null, configuration });
    }
  }
  const win = { MediaSource: FakeMediaSource, HTMLMediaElement: FakeHTMLMediaElement, MediaCapabilities: FakeMediaCapabilities };
  return { win, calls, FakeMediaSource, FakeHTMLMediaElement, FakeMediaCapabilities };
}

function install(policy: CodecPolicy, browser = createFakeBrowser()) {
  const cell: PolicyCell = createPolicyCell();
  cell.adoptFromCarrier(policy);
  const oddities: string[] = [];
  const installed = installCodecAnswerOverrides(browser.win, cell, (what) => oddities.push(what));
  return { browser, cell, oddities, installed };
}

describe("отказ при политике «только H.264»", () => {
  const h264Only = refuse(["av1", "vp9", "hevc", "vp8"]);

  it("isTypeSupported: AV1/VP9/HEVC — false, H.264 и звук — как у браузера", () => {
    const { browser } = install(h264Only);
    const ask = (type: string) => browser.FakeMediaSource.isTypeSupported(type);
    assert.equal(ask(AV1), false);
    assert.equal(ask('video/webm; codecs="vp09.00.10.08"'), false);
    assert.equal(ask('video/mp4; codecs="hvc1.1.6.L93.B0"'), false);
    assert.equal(ask('video/webm; codecs="vp8"'), false);
    assert.equal(ask(H264), true);
    assert.equal(ask('audio/webm; codecs="opus"'), true);
    assert.equal(ask("video/mp4"), true);
  });

  it("canPlayType: отказ — пустая строка, остальное — ответ браузера", () => {
    const { browser } = install(h264Only);
    const element = new browser.FakeHTMLMediaElement();
    assert.equal(element.canPlayType(AV1), "");
    assert.equal(element.canPlayType('video/webm; codecs="vp9"'), "");
    assert.equal(element.canPlayType(H264), "probably");
    assert.equal(element.canPlayType("audio/mpeg"), "probably");
  });

  it("decodingInfo: отказ сохраняет форму ответа браузера (configuration остаётся), согласие — тот же ответ", async () => {
    const { browser } = install(h264Only);
    const media = new browser.FakeMediaCapabilities();
    const refusedConfig = { type: "media-source", video: { contentType: AV1, width: 1920, height: 1080, bitrate: 1, framerate: 30 } };
    const refused = (await media.decodingInfo(refusedConfig)) as Record<string, unknown>;
    assert.deepEqual(Object.keys(refused).sort(), ["configuration", "keySystemAccess", "powerEfficient", "smooth", "supported"]);
    assert.equal(refused["supported"], false);
    assert.equal(refused["smooth"], false);
    assert.equal(refused["powerEfficient"], false);
    assert.equal(refused["keySystemAccess"], null);
    assert.equal(refused["configuration"], refusedConfig);

    const allowedConfig = { type: "file", video: { contentType: H264 }, audio: { contentType: "audio/mp4; codecs=mp4a.40.2" } };
    const allowed = (await media.decodingInfo(allowedConfig)) as Record<string, unknown>;
    assert.equal(allowed["supported"], true);
    assert.equal(allowed["smooth"], true);
  });

  it("decodingInfo: тип webrtc и конфигурация без contentType остаются за браузером", async () => {
    const { browser } = install(h264Only);
    const media = new browser.FakeMediaCapabilities();
    const webrtc = (await media.decodingInfo({ type: "webrtc", video: { contentType: "video/AV1" } })) as Record<string, unknown>;
    assert.equal(webrtc["supported"], true);
    const bare = (await media.decodingInfo({ type: "file" })) as Record<string, unknown>;
    assert.equal(bare["supported"], true);
  });

  it("оригинал вызывается ВСЕГДА, с тем же this и аргументами (в том числе при отказе)", () => {
    const { browser } = install(h264Only);
    browser.FakeMediaSource.isTypeSupported(AV1);
    assert.deepEqual(browser.calls.isTypeSupported, [{ self: browser.FakeMediaSource, args: [AV1] }]);
    const element = new browser.FakeHTMLMediaElement();
    element.canPlayType(AV1);
    assert.deepEqual(browser.calls.canPlayType, [{ self: element, args: [AV1] }]);
  });

  it("исключения оригинала доходят до страницы как есть: чужой this, нет аргумента, отказ decodingInfo", async () => {
    const { browser } = install(h264Only);
    assert.throws(() => (browser.FakeHTMLMediaElement.prototype.canPlayType as (type: string) => string).call({}, AV1), /Illegal invocation/);
    assert.throws(() => (browser.FakeMediaSource.isTypeSupported as () => boolean)(), /1 argument required/);
    await assert.rejects(new browser.FakeMediaCapabilities().decodingInfo(undefined as never), /bad configuration/);
  });

  it("мусор от страницы не бросает наш код: не строки, геттеры, огромные строки", async () => {
    const { browser, oddities } = install(h264Only);
    const element = new browser.FakeHTMLMediaElement();
    for (const junk of [undefined, null, 42, {}, [], Symbol.iterator.description]) {
      assert.doesNotThrow(() => element.canPlayType(junk as never));
    }
    const hostile = {
      type: "file",
      get video(): never {
        throw new Error("hostile getter");
      },
    };
    // Геттер бросает и у нас (решение не принято -> оригинал), и у оригинала не вызывается — важно, что наш слой не падает.
    const answer = (await new browser.FakeMediaCapabilities().decodingInfo(hostile)) as Record<string, unknown>;
    assert.equal(answer["supported"], true);
    assert.equal(element.canPlayType("x".repeat(100_000)), "probably");
    // Сбой чтения геттера записан как странность нашего слоя, а не брошен странице.
    assert.deepEqual(oddities, ["codec answer rewrite failed: decodingInfo"]);
  });

  it("обёртки похожи на оригинал: имя, length, не конструктор, те же флаги свойства", () => {
    const browser = createFakeBrowser();
    const before = {
      source: Object.getOwnPropertyDescriptor(browser.FakeMediaSource, "isTypeSupported")!,
      element: Object.getOwnPropertyDescriptor(browser.FakeHTMLMediaElement.prototype, "canPlayType")!,
    };
    install(h264Only, browser);
    const after = {
      source: Object.getOwnPropertyDescriptor(browser.FakeMediaSource, "isTypeSupported")!,
      element: Object.getOwnPropertyDescriptor(browser.FakeHTMLMediaElement.prototype, "canPlayType")!,
    };
    for (const key of ["source", "element"] as const) {
      assert.notEqual(after[key].value, before[key].value, "метод заменён");
      assert.equal(after[key].value.name, before[key].value.name);
      assert.equal(after[key].value.length, before[key].value.length);
      assert.equal(after[key].writable, before[key].writable);
      assert.equal(after[key].enumerable, before[key].enumerable);
      assert.equal(after[key].configurable, before[key].configurable);
      assert.throws(() => new after[key].value(), TypeError, "не конструктор");
    }
  });
});

describe("политика «VP9, но не AV1» (prefer_vp9)", () => {
  it("AV1 отвергается, VP9/VP8/H.264/HEVC и неопознанное видео остаются за браузером", () => {
    const { browser } = install(refuse(["av1"]));
    const ask = (type: string) => browser.FakeMediaSource.isTypeSupported(type);
    assert.equal(ask(AV1), false);
    assert.equal(ask('video/webm; codecs="vp09.00.10.08"'), true);
    assert.equal(ask('video/webm; codecs="vp8"'), true, "VP8 не трогаем: о нём никто не просил");
    assert.equal(ask('video/ogg; codecs="theora"'), true);
    assert.equal(ask('video/mp4; codecs="hvc1.1.6.L93.B0"'), true);
    assert.equal(ask(H264), true);
    assert.equal(ask("video/webm"), true, "голый WebM — семейство VP9");
  });
});

describe("HLS отказывает всегда (решение 64)", () => {
  for (const [label, policy] of [
    ["прозрачная", refuse([])],
    ["только H.264", refuse(["av1", "vp9", "hevc", "vp8"])],
    ["только AV1", refuse(["av1"])],
  ] as const) {
    it(`политика «${label}»: isTypeSupported/canPlayType/decodingInfo не признают плейлисты`, async () => {
      const { browser } = install(policy);
      for (const mime of [HLS, "application/x-mpegURL", "audio/mpegurl", "audio/x-mpegurl", `${HLS}; codecs="avc1.4d401f"`]) {
        assert.equal(browser.FakeMediaSource.isTypeSupported(mime), false, mime);
        assert.equal(new browser.FakeHTMLMediaElement().canPlayType(mime), "", mime);
      }
      const info = (await new browser.FakeMediaCapabilities().decodingInfo({ type: "file", video: { contentType: HLS } })) as Record<string, unknown>;
      assert.equal(info["supported"], false);
    });
  }
});

describe("прозрачная политика", () => {
  it("кроме HLS ответы браузера не меняются", () => {
    const { browser } = install(refuse([]));
    assert.equal(browser.FakeMediaSource.isTypeSupported(AV1), true);
    assert.equal(new browser.FakeHTMLMediaElement().canPlayType(AV1), "probably");
  });

  it("политика читается при каждом вызове: ответ курьера после установки меняет ответы", () => {
    const browser = createFakeBrowser();
    const cell = createPolicyCell();
    installCodecAnswerOverrides(browser.win, cell, () => {});
    assert.equal(browser.FakeMediaSource.isTypeSupported(AV1), true);
    cell.adoptFromCarrier(refuse(["av1", "vp9", "hevc", "vp8"]));
    assert.equal(browser.FakeMediaSource.isTypeSupported(AV1), false);
  });
});

describe("отсутствующие части окружения", () => {
  it("нет MediaSource / HTMLMediaElement / MediaCapabilities — ничего не трогаем и не бросаем", () => {
    const cell = createPolicyCell();
    assert.deepEqual(installCodecAnswerOverrides({}, cell, () => {}), { isTypeSupported: false, canPlayType: false, decodingInfo: false });
  });

  it("установка не меняет то, чем не владеет: прочие методы и статики остаются теми же", () => {
    const browser = createFakeBrowser();
    class WithExtras extends browser.FakeMediaSource {
      static other(): string {
        return "other";
      }
    }
    const other = WithExtras.other;
    install(refuse(["av1", "vp9", "hevc", "vp8"]), { ...browser, win: { ...browser.win, MediaSource: WithExtras } });
    assert.equal(WithExtras.other, other);
  });
});

describe("соседство с чужими обёртками (блокировщики, другие расширения)", () => {
  /** Чужая обёртка, которая фиксирует, что её вызвали, и всё пропускает. */
  function wrapForeign(owner: object, name: string, log: string[]): void {
    const descriptor = Object.getOwnPropertyDescriptor(owner, name)!;
    const inner = descriptor.value as (...args: unknown[]) => unknown;
    Object.defineProperty(owner, name, {
      ...descriptor,
      value: function foreign(this: unknown, ...args: unknown[]) {
        log.push(`foreign:${name}`);
        return Reflect.apply(inner, this, args);
      },
    });
  }

  it("чужая обёртка стояла ДО нас: вызов проходит через обеих, ответ отказа наш", () => {
    const browser = createFakeBrowser();
    const log: string[] = [];
    wrapForeign(browser.FakeMediaSource, "isTypeSupported", log);
    install(refuse(["av1", "vp9", "hevc", "vp8"]), browser);
    assert.equal(browser.FakeMediaSource.isTypeSupported(AV1), false);
    assert.deepEqual(log, ["foreign:isTypeSupported"]);
    assert.equal(browser.calls.isTypeSupported.length, 1, "оригинал браузера всё же вызван");
  });

  it("чужая обёртка встала ПОСЛЕ нас: она зовёт нашу, отказ сохраняется", () => {
    const browser = createFakeBrowser();
    install(refuse(["av1", "vp9", "hevc", "vp8"]), browser);
    const log: string[] = [];
    wrapForeign(browser.FakeMediaSource, "isTypeSupported", log);
    wrapForeign(browser.FakeHTMLMediaElement.prototype, "canPlayType", log);
    assert.equal(browser.FakeMediaSource.isTypeSupported(AV1), false);
    assert.equal(new browser.FakeHTMLMediaElement().canPlayType(AV1), "");
    assert.deepEqual(log, ["foreign:isTypeSupported", "foreign:canPlayType"]);
  });

  it("чужая обёртка вернула своё (true) ДО нас: мы всё равно отказываем по политике", () => {
    const browser = createFakeBrowser();
    Object.defineProperty(browser.FakeMediaSource, "isTypeSupported", { value: () => true, writable: true, configurable: true });
    install(refuse(["av1", "vp9", "hevc", "vp8"]), browser);
    assert.equal(browser.FakeMediaSource.isTypeSupported(AV1), false);
    assert.equal(browser.FakeMediaSource.isTypeSupported(H264), true);
  });
});
