// Ввод человека для режима 2 (сессия 12a): оригинальные геттеры активации и доверенные события ввода.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  FALLBACK_ACTIVATION_WINDOW_MS,
  installViewerActivation,
  ViewerActivation,
  type ActivationWindow,
} from "../../src/page/auto-handoff/viewer-activation.ts";

type Listener = (event: unknown) => void;

/** Окно с `navigator.userActivation`, как у браузера: геттеры живут на прототипах. */
function windowWithActivation() {
  const state = { active: false };
  class UserActivation {
    get isActive(): boolean {
      return state.active;
    }
  }
  class Navigator {
    readonly #activation = new UserActivation();
    get userActivation(): UserActivation {
      return this.#activation;
    }
  }
  const win: ActivationWindow & object = { navigator: new Navigator(), Navigator, UserActivation };
  return { win, state, Navigator, UserActivation };
}

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

function setup(win: ActivationWindow & object) {
  const clock = { nowMs: 10_000 };
  const oddities: string[] = [];
  const { channel, fire } = fakeChannel();
  const activation = installViewerActivation(win, channel, () => clock.nowMs, (line) => void oddities.push(line));
  return { activation, clock, oddities, fire };
}

describe("временная активация браузера", () => {
  it("читается оригинальным геттером: подмена страницей не обманывает", () => {
    const { win, state, Navigator, UserActivation } = windowWithActivation();
    const { activation } = setup(win);
    state.active = true;
    assert.equal(activation.hasTransientActivation(), true);
    // Страница позже притворяется, что жеста нет (и наоборот): наш читатель идёт мимо.
    Object.defineProperty(UserActivation.prototype, "isActive", { get: () => false });
    Object.defineProperty(Navigator.prototype, "userActivation", { get: () => ({ isActive: false }) });
    assert.equal(activation.hasTransientActivation(), true);
    state.active = false;
    assert.equal(activation.hasTransientActivation(), false);
  });

  it("геттер бросил: активации нет, страница ничего не получила, след оставлен один раз", () => {
    const oddities: string[] = [];
    const failing = new ViewerActivation({
      readTransientActivation: () => {
        throw new Error("brand check failed");
      },
      now: () => 0,
      reportOddity: (line) => void oddities.push(line),
    });
    assert.equal(failing.hasTransientActivation(), false);
    assert.equal(failing.hasTransientActivation(), false);
    assert.equal(oddities.length, 1);
  });
});

describe("свои отметки доверенного ввода", () => {
  it("доверенный клик мышью, касание и клавиша записываются; Escape и чужие события — нет", () => {
    const { win } = windowWithActivation();
    const { activation, clock, fire } = setup(win);
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
    const { win } = windowWithActivation();
    const { activation, fire, oddities } = setup(win);
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

describe("браузера без `userActivation`", () => {
  it("запасной путь: свой ввод моложе окна считается активацией, старше — нет", () => {
    const { activation, clock, fire } = setup({});
    assert.equal(activation.hasTransientActivation(), false, "ввода не было");
    fire("keydown", { isTrusted: true, key: "a" });
    clock.nowMs += FALLBACK_ACTIVATION_WINDOW_MS;
    assert.equal(activation.hasTransientActivation(), true);
    clock.nowMs += 1;
    assert.equal(activation.hasTransientActivation(), false);
  });
});
