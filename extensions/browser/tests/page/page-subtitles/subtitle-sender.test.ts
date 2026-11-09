// Отправитель событий субтитров (13b2): исходы «ушло / счётчик отказал / порт отказал» не сливаются в один bool.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { captureBinaryIntrinsics } from "../../../src/page/binary-view.ts";
import { createForwardByteMeter } from "../../../src/page/forward-byte-meter.ts";
import { createSubtitleSender } from "../../../src/page/page-subtitles/subtitle-sender.ts";
import type { PageSubtitleEvent } from "../../../src/shared/page-subtitle-contract.ts";

const binary = captureBinaryIntrinsics({ Uint8Array, ArrayBuffer, DataView });
const NAMING = { language: "en", label: null, auto_generated: false } as const;
const fields = (bytes: Uint8Array) => ({ key: 1, naming: NAMING, format_hint: "web_vtt", origin: "page_download", display: "showing", bytes }) as const;

function setup(meterBytesPerSecond?: number, failSink = false) {
  const sent: { event: PageSubtitleEvent; transfer: readonly Transferable[] }[] = [];
  const oddities: string[] = [];
  let ticks = 0;
  const meter = createForwardByteMeter(() => ticks, meterBytesPerSecond);
  const sender = createSubtitleSender({ binary, meter, reportOddity: (what) => oddities.push(what) }, (event, transfer) => {
    if (failSink) throw new Error("port is closed");
    sent.push({ event, transfer });
  });
  return { sender, sent, oddities, advance: (ms: number) => void (ticks += ms) };
}

describe("отправитель субтитров", () => {
  it("документ уходит точной копией с transfer, исходник цел", () => {
    const { sender, sent } = setup();
    const source = new Uint8Array(new ArrayBuffer(100), 10, 5).fill(7);
    assert.equal(sender.sendDocument(fields(source)), "sent");
    const event = sent[0]?.event;
    assert.ok(event !== undefined && event.kind === "subtitle_document");
    assert.deepEqual([...event.bytes], [7, 7, 7, 7, 7]);
    assert.equal(event.bytes.buffer.byteLength, 5, "буфер ровно по размеру документа");
    assert.deepEqual(sent[0]?.transfer, [event.bytes.buffer]);
    assert.equal(source.length, 5, "исходный вид не тронут");
  });

  it("счётчик копий отказал: исход refused, в порт ничего, одна запись о странности", () => {
    const { sender, sent, oddities } = setup(10);
    assert.equal(sender.sendDocument(fields(new Uint8Array(10).fill(1))), "sent");
    assert.equal(sender.sendDocument(fields(new Uint8Array(10).fill(2))), "refused", "ведро пусто");
    assert.equal(sent.length, 1);
    assert.deepEqual(oddities, ["a subtitle document was refused: rate_exceeded"]);
  });

  it("порт отказал: исход failed, исключение наружу не уходит", () => {
    const { sender, oddities } = setup(undefined, true);
    assert.equal(sender.sendDocument(fields(new Uint8Array(4).fill(1))), "failed");
    assert.equal(sender.sendTrackSource({ key: 2, naming: NAMING, display: "not_showing", url: "https://cdn.example/a.vtt", referrer_origin: null, referrer_policy: "no-referrer" }), "failed");
    assert.deepEqual(oddities, ["a subtitle event could not be sent", "a subtitle event could not be sent"]);
  });

  it("track_source не тратит счётчик байт и не несёт transfer", () => {
    const { sender, sent } = setup(1);
    const outcome = sender.sendTrackSource({ key: 2, naming: NAMING, display: "showing", url: "https://cdn.example/a.vtt", referrer_origin: "https://site.example", referrer_policy: "origin" });
    assert.equal(outcome, "sent");
    assert.deepEqual(sent[0]?.transfer, []);
    assert.equal(sent[0]?.event.kind, "subtitle_track_source");
  });
});
