// Ввод человека для режима 2 (сессии 12a, решение 262): доверенные события ввода и нажатия мыши с точкой.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { installViewerActivation } from "../../src/page/auto-handoff/viewer-activation.ts";

type Listener = (event: unknown) => void;

/** Слушатели окна, снятые «оригинальным» addEventListener. */
function fakeChannel() {
  const listeners = new Map<string, Listener[]>();
  return {
    channel: {
      addListener: (_target: object, type: string, listener: Listener, capture: boolean) => {
        assert.equal(capture, true, "ввод ловим в захвате, раньше страницы");
        listeners.set(type, [...(listeners.get(type) ?? []), listener]);
      },
    },
    fire: (type: string, event: object) => (listeners.get(type) ?? []).forEach((listener) => listener(event)),
  };
}

function setup() {
  const clock = { nowMs: 10_000 };
  const oddities: string[] = [];
  const { channel, fire } = fakeChannel();
  const activation = installViewerActivation({}, channel, () => clock.nowMs, (line) => void oddities.push(line));
  return { activation, clock, oddities, fire };
}

describe("свои отметки доверенного ввода", () => {
  it("доверенный клик мышью, касание и клавиша записываются; Escape и чужие события — нет", () => {
    const { activation, clock, fire } = setup();
    assert.equal(activation.lastTrustedInputAtMs(), Number.NEGATIVE_INFINITY);

    fire("pointerdown", { isTrusted: false, pointerType: "mouse" });
    assert.equal(activation.lastTrustedInputAtMs(), Number.NEGATIVE_INFINITY, "скриптовое событие не ввод");

    fire("pointerdown", { isTrusted: true, pointerType: "mouse" });
    assert.equal(activation.lastTrustedInputAtMs(), 10_000);

    clock.nowMs = 11_000;
    fire("pointerup", { isTrusted: true, pointerType: "mouse" });
    assert.equal(activation.lastTrustedInputAtMs(), 10_000, "у мыши активирует pointerdown, а не pointerup");

    clock.nowMs = 12_000;
    fire("pointerup", { isTrusted: true, pointerType: "touch" });
    assert.equal(activation.lastTrustedInputAtMs(), 12_000);

    clock.nowMs = 13_000;
    fire("keydown", { isTrusted: true, key: "Escape" });
    assert.equal(activation.lastTrustedInputAtMs(), 12_000, "Escape не жест");
    fire("keydown", { isTrusted: true, key: " " });
    assert.equal(activation.lastTrustedInputAtMs(), 13_000);
  });

  it("событие с бросающим геттером не долетает до страницы и оставляет след", () => {
    const { activation, fire, oddities } = setup();
    const hostile = {
      isTrusted: true,
      get pointerType(): string {
        throw new Error("hostile getter");
      },
    };
    fire("pointerdown", hostile);
    assert.equal(activation.lastTrustedInputAtMs(), Number.NEGATIVE_INFINITY);
    assert.deepEqual(oddities, ["could not read an input event"]);
  });
});

describe("нажатия мыши (решение 262)", () => {
  it("доверенное нажатие основной кнопкой записывает момент и точку; правая кнопка и скрипт — нет", () => {
    const { activation, clock, fire } = setup();
    assert.equal(activation.lastPointerPress(), null);
    fire("pointerdown", { isTrusted: false, button: 0, clientX: 5, clientY: 6 });
    assert.equal(activation.lastPointerPress(), null, "скриптовое нажатие не клик человека");
    fire("pointerdown", { isTrusted: true, button: 2, clientX: 5, clientY: 6 });
    assert.equal(activation.lastPointerPress(), null, "правая кнопка не Play");
    fire("pointerdown", { isTrusted: true, button: 0, pointerType: "mouse", clientX: 120.5, clientY: 80 });
    assert.deepEqual(activation.lastPointerPress(), { atMs: 10_000, x: 120.5, y: 80 });
    assert.equal(activation.lastTrustedInputAtMs(), 10_000, "нажатие — тоже доверенный ввод");
    clock.nowMs = 10_300;
    fire("mousedown", { isTrusted: true, button: 0, clientX: 7, clientY: 9 });
    assert.deepEqual(activation.lastPointerPress(), { atMs: 10_300, x: 7, y: 9 }, "запасной путь mousedown");
  });

  it("клавиша — доверенный ввод, но не нажатие мыши: по клавише запуск не засчитывается", () => {
    const { activation, fire } = setup();
    fire("keydown", { isTrusted: true, key: " " });
    assert.equal(activation.lastTrustedInputAtMs(), 10_000);
    assert.equal(activation.lastPointerPress(), null);
  });

  it("точка не число (NaN, нет поля) — нажатие не записывается; бросающий геттер оставляет след", () => {
    const { activation, fire, oddities } = setup();
    fire("pointerdown", { isTrusted: true, button: 0, clientX: Number.NaN, clientY: 1 });
    fire("pointerdown", { isTrusted: true, button: 0 });
    assert.equal(activation.lastPointerPress(), null);
    fire("mousedown", {
      isTrusted: true,
      get button(): number {
        throw new Error("hostile getter");
      },
    });
    assert.equal(activation.lastPointerPress(), null);
    assert.deepEqual(oddities, ["could not read a pointer press"]);
  });
});
