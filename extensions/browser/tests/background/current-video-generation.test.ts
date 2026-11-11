// Владелец «поколения текущего видео» (сессия 16a2, решение 311): пометка тика часов `this_video` /
// `previous_video` / `unknown` по номеру поколения, нумерации сеанса и мигу старта видео.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { classifyClockGeneration, createCurrentVideoGeneration } from "../../src/background/handoff/current-video-generation.ts";
import { createStreamNumbering } from "../../src/background/handoff/stream-numbering.ts";
import { CURRENT_PROTOCOL_VERSION, PROTOCOL_VERSION_1_9, PROTOCOL_VERSION_1_10, type ProtocolVersion } from "../../src/protocol/index.ts";

/** Настоящая нумерация сеанса и владелец поверх неё; `agreed` — общая версия протокола. */
function sessionGenerations(agreed: ProtocolVersion = CURRENT_PROTOCOL_VERSION) {
  const numbering = createStreamNumbering();
  const document = numbering.openDocument();
  const owner = createCurrentVideoGeneration({ numbering, agreedProtocol: () => agreed });
  return { numbering, document, owner };
}

/** Номер только что открытого поколения (тест знает, что номера не кончились). */
function started(lookup: { readonly kind: string; readonly generation?: number }): number {
  assert.ok(lookup.kind === "started" && lookup.generation !== undefined, JSON.stringify(lookup));
  return lookup.generation;
}

describe("пометка тика по номеру поколения: чистое правило", () => {
  it("нет номера — unknown при любых границах", () => {
    assert.equal(classifyClockGeneration(undefined, { latest: undefined, floor: undefined }), "unknown");
    assert.equal(classifyClockGeneration(undefined, { latest: 3, floor: 1 }), "unknown");
  });

  it("номер больше последнего объявленного (или поколений нет) — unknown, а не прошлое видео", () => {
    assert.equal(classifyClockGeneration(1, { latest: undefined, floor: undefined }), "unknown");
    assert.equal(classifyClockGeneration(4, { latest: 3, floor: 1 }), "unknown");
    assert.equal(classifyClockGeneration(4, { latest: 3, floor: 3 }), "unknown", "и в окне переезда");
  });

  it("последний номер после границы — this_video; меньше последнего — previous_video", () => {
    assert.equal(classifyClockGeneration(1, { latest: 1, floor: undefined }), "this_video");
    assert.equal(classifyClockGeneration(3, { latest: 3, floor: 1 }), "this_video");
    assert.equal(classifyClockGeneration(2, { latest: 3, floor: 1 }), "previous_video", "прошлый MediaSource того же видео");
    assert.equal(classifyClockGeneration(1, { latest: 3, floor: 1 }), "previous_video");
  });

  it("окно переезда: последний номер равен границе — это поколение прошлого видео", () => {
    assert.equal(classifyClockGeneration(2, { latest: 2, floor: 2 }), "previous_video");
  });
});

describe("владелец поколения текущего видео на нумерации сеанса", () => {
  it("первое видео сеанса: до объявления номер неизвестен, после — this_video", () => {
    const { document, owner } = sessionGenerations();
    owner.videoStarted();
    assert.equal(owner.clockRelationOf(1), "unknown", "плеер назвал номер, которого фон ещё не объявлял");
    const first = started(document.generationOf("ms-1"));
    assert.equal(owner.clockRelationOf(first), "this_video");
    assert.equal(owner.clockRelationOf(undefined), "unknown");
    assert.equal(owner.latestGeneration(), first);
  });

  it("переезд: старый номер в окне до объявления — previous_video, новый после объявления — this_video", () => {
    const { document, owner } = sessionGenerations();
    owner.videoStarted();
    const banner = started(document.startFileGeneration());
    assert.equal(owner.clockRelationOf(banner), "this_video");
    // Тень встала на фильм; его поколение объявит первое событие журнала MSE.
    owner.videoStarted();
    assert.equal(owner.clockRelationOf(banner), "previous_video", "конец баннера в окне переезда — часы прошлого видео");
    const film = started(document.generationOf("ms-1"));
    assert.equal(owner.clockRelationOf(film), "this_video");
    assert.equal(owner.clockRelationOf(banner), "previous_video");
  });

  it("новый MediaSource на том же элементе без переезда: прошлое поколение — previous_video", () => {
    const { document, owner } = sessionGenerations();
    owner.videoStarted();
    const first = started(document.generationOf("ms-1"));
    const second = started(document.generationOf("ms-2"));
    assert.equal(owner.clockRelationOf(second), "this_video");
    assert.equal(owner.clockRelationOf(first), "previous_video");
  });

  it("файл, HLS и MSE считаются одной нумерацией: номер тика = номер объявления", () => {
    const { numbering, document, owner } = sessionGenerations();
    owner.videoStarted();
    const file = started(document.startFileGeneration());
    assert.equal(owner.clockRelationOf(file), "this_video");
    // HLS открывает поколение тем же `startFileGeneration` (page-event-translator.ts), здесь — в другом документе
    // (другой фрейм): свои ключи, общий счёт поколений. Путь через весь фон проверяет clock-relation-session.test.ts.
    const hls = started(numbering.openDocument().startFileGeneration());
    assert.ok(hls > file);
    assert.equal(owner.clockRelationOf(hls), "this_video");
    assert.equal(owner.clockRelationOf(file), "previous_video");
  });

  it("сторона ниже 1.10 номера не шлёт: присланному всё равно не верим — unknown", () => {
    const { document, owner } = sessionGenerations(PROTOCOL_VERSION_1_9);
    owner.videoStarted();
    const first = started(document.generationOf("ms-1"));
    assert.equal(owner.clockRelationOf(first), "unknown");
    const agreed110 = sessionGenerations(PROTOCOL_VERSION_1_10);
    agreed110.owner.videoStarted();
    assert.equal(agreed110.owner.clockRelationOf(started(agreed110.document.generationOf("ms-1"))), "this_video");
  });

  it("пометка ничего не меняет: ни нумерацию, ни границу", () => {
    const { numbering, document, owner } = sessionGenerations();
    owner.videoStarted();
    const first = started(document.generationOf("ms-1"));
    for (let tick = 0; tick < 5; tick += 1) owner.clockRelationOf(first + tick);
    assert.equal(numbering.latestGeneration(), first);
    assert.equal(owner.clockRelationOf(first), "this_video", "граница не сдвинулась от вопросов");
  });
});
