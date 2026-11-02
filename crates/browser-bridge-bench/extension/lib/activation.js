// TEST-ONLY (стенд моста, сессия 08). Общий для MAIN- и ISOLATED-мира код: активен ли перехват
// в этом фрейме и какой у прогона `run`. Обычный скрипт без модулей: контент-скрипты не умеют
// import, поэтому файлы перечислены в manifest `js` и делят глобальное пространство __fpBench
// (в node-тестах тот же файл подключается через module.exports).
(function (root) {
  "use strict";
  // Тег postMessage между MAIN-хуком и ISOLATED-мостом.
  const HOOK_CHANNEL = "fastiplayer-bench-hook-v1";
  const RUN_PATTERN = /^[a-z0-9-]{1,64}$/;

  // Разбор query: handoff=1 включает перехват; run — идентификатор прогона для журнала.
  function parseBenchParams(search) {
    const params = new URLSearchParams(search || "");
    const run = params.get("run");
    return { handoff: params.get("handoff") === "1", run: run && RUN_PATTERN.test(run) ? run : null };
  }

  // Активация по URL фрейма ИЛИ верхней страницы. Чужой origin верхней страницы кидает
  // SecurityError — тогда решает только URL самого фрейма (страница стенда передаёт
  // handoff=1 и run вниз в URL iframe).
  function resolveActivation(win) {
    const own = parseBenchParams(win.location.search);
    let top = { handoff: false, run: null };
    try {
      if (win.top && win.top !== win) top = parseBenchParams(win.top.location.search);
    } catch (crossOrigin) {
      // ожидаемо для iframe с чужого origin
    }
    return { handoff: own.handoff || top.handoff, run: own.run || top.run };
  }

  const api = { HOOK_CHANNEL, parseBenchParams, resolveActivation };
  root.__fpBench = Object.assign(root.__fpBench || {}, api);
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(globalThis);
