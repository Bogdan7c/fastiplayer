// Сбор проверок страницы и отправка отчёта стенду.
//
// Контракт: страница шлёт РОВНО ОДИН отчёт `POST <report_origin>/bench/report`
// со статусом pass | fail | unsupported и списком проверок с числами в `detail`.
// Параметры URL: run (id прогона), report_origin (куда слать; по умолчанию свой origin),
// mode=handoff (режим передачи: страницей командует робот стенда, см. handoff-mode.js).

/** Общий бюджет страницы, мс (в брифе: вся страница ≤ ~25 с). */
export const PAGE_BUDGET_MS = 25000;
/**
 * Бюджет страницы в режиме передачи: страницей командует робот (передача, команды плеера,
 * ожидание сводок плеера раз в 10 с), поэтому 25 с мало. Робот сам ограничен таймаутами.
 */
export const HANDOFF_PAGE_BUDGET_MS = 240000;
import { HANDOFF_MODE, createRealFrameProbe } from './handoff-mode.js';

/** Идентификатор прогона, если страницу открыли руками. */
const MANUAL_RUN_ID = 'manual';

/** Бросается сценарием, когда форма в этом браузере принципиально не существует. */
export class UnsupportedError extends Error {}

export function pageParams() {
  const params = new URLSearchParams(location.search);
  return {
    run: params.get('run') || MANUAL_RUN_ID,
    reportOrigin: params.get('report_origin') || location.origin,
    mode: params.get('mode') || 'plain',
    handoff: params.get('mode') === HANDOFF_MODE,
    otherOrigin: params.get('other_origin') || null,
    raw: params,
  };
}

async function postJson(origin, path, body) {
  try {
    await fetch(`${origin}${path}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
      keepalive: true,
    });
  } catch (error) {
    // Отправить нечем — остаётся заголовок страницы; наружу не глотаем, а пишем в консоль.
    console.error('bench: post failed', path, error);
  }
}

/** Контекст сценария: проверки, журнал, ошибки страницы. */
class ScenarioContext {
  constructor(scenario, params) {
    this.scenario = scenario;
    this.params = params;
    this.checks = [];
    /** Именованные числа отчёта: сценарий стенда сравнивает их с порогами. */
    this.metrics = {};
    /** Настоящий счётчик кадров мимо тени (только в режиме передачи). */
    this.realFrames = null;
    this.pageErrors = [];
    this.startedAt = performance.now();
  }

  /** Записывает проверку; возвращает `ok`, чтобы удобно ветвить код. */
  check(name, ok, detail) {
    this.checks.push({ name, ok: Boolean(ok), detail: String(detail) });
    this.log(`check ${ok ? 'ok' : 'FAIL'} ${name}: ${detail}`);
    return Boolean(ok);
  }

  /** Записывает число в отчёт (конечное; всё остальное — ошибка страницы, а не молчаливый 0). */
  metric(name, value) {
    if (!Number.isFinite(value)) {
      this.check(`metric-${name}`, false, `not a finite number: ${value}`);
      return;
    }
    this.metrics[name] = value;
    this.log(`metric ${name}=${value}`);
  }

  /** Строка в журнал стенда (`POST /bench/log`, source=page). */
  log(line) {
    console.log(`[${this.scenario}] ${line}`);
    return postJson(this.params.reportOrigin, '/bench/log', {
      source: 'page',
      run: this.params.run,
      line: `${this.scenario}: ${line}`,
    });
  }

  elapsedMs() {
    return Math.round(performance.now() - this.startedAt);
  }
}

/** Ловит ошибки окна и необработанные отказы промисов — они попадают в отчёт. */
function collectPageErrors(context) {
  window.addEventListener('error', (event) => {
    context.pageErrors.push(`error: ${event.message}`);
  });
  window.addEventListener('unhandledrejection', (event) => {
    context.pageErrors.push(`unhandledrejection: ${event.reason?.message ?? event.reason}`);
  });
}

function withDeadline(promise, budgetMs) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`page budget ${budgetMs} ms exceeded`)), budgetMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function decideStatus(context, unsupported) {
  if (unsupported) return 'unsupported';
  const allOk = context.checks.length > 0 && context.checks.every((entry) => entry.ok);
  return allOk ? 'pass' : 'fail';
}

/**
 * Запускает сценарий: автостарт, общий таймаут, один отчёт, заголовок для человека.
 * `body(context)` делает проверки через `context.check`.
 * Если `context.params.reportToParent` задан в `options`, отчёт уходит в `window.parent`
 * (для внутренней страницы iframe), а не на сервер.
 */
export async function runScenario(scenario, body, options = {}) {
  const params = pageParams();
  const context = new ScenarioContext(scenario, params);
  collectPageErrors(context);
  document.title = `RUN ${scenario}`;
  let unsupported = false;
  const budgetMs = options.budgetMs ?? (params.handoff ? HANDOFF_PAGE_BUDGET_MS : PAGE_BUDGET_MS);
  try {
    // Чистый счётчик кадров создаём ДО плеера страницы (в режиме передачи).
    if (params.handoff) context.realFrames = await createRealFrameProbe();
    await withDeadline(body(context), budgetMs);
  } catch (error) {
    if (error instanceof UnsupportedError) {
      unsupported = true;
      context.check('unsupported-reason', true, error.message);
    } else {
      context.check('scenario-completed', false, `${error?.name ?? 'Error'}: ${error?.message ?? error}`);
    }
  }
  if (!unsupported) {
    context.check(
      'no-page-errors',
      context.pageErrors.length === 0,
      context.pageErrors.length === 0 ? '0 uncaught errors' : context.pageErrors.join(' | '),
    );
  }
  const report = {
    run: params.run,
    scenario,
    status: decideStatus(context, unsupported),
    checks: context.checks,
    user_agent: navigator.userAgent,
    elapsed_ms: context.elapsedMs(),
    metrics: context.metrics,
  };
  document.title = `${report.status.toUpperCase()} ${scenario} (${report.elapsed_ms} ms)`;
  if (options.reportToParent) {
    window.parent.postMessage({ benchInnerReport: report }, '*');
  } else {
    await postJson(params.reportOrigin, '/bench/report', report);
  }
  document.body.dataset.benchStatus = report.status;
  return report;
}

/** Отправляет произвольный отчёт (для внешней страницы iframe, которая собирает чужие проверки). */
export { postJson };
