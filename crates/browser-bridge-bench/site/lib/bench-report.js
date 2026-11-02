// Сбор проверок страницы и отправка отчёта стенду.
//
// Контракт: страница шлёт РОВНО ОДИН отчёт `POST <report_origin>/bench/report`
// со статусом pass | fail | unsupported и списком проверок с числами в `detail`.
// Параметры URL: run (id прогона), report_origin (куда слать; по умолчанию свой origin).

/** Общий бюджет страницы, мс (в брифе: вся страница ≤ ~25 с). */
export const PAGE_BUDGET_MS = 25000;
/** Идентификатор прогона, если страницу открыли руками. */
const MANUAL_RUN_ID = 'manual';

/** Бросается сценарием, когда форма в этом браузере принципиально не существует. */
export class UnsupportedError extends Error {}

export function pageParams() {
  const params = new URLSearchParams(location.search);
  return {
    run: params.get('run') || MANUAL_RUN_ID,
    reportOrigin: params.get('report_origin') || location.origin,
    handoff: params.get('handoff') === '1',
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
    this.pageErrors = [];
    this.startedAt = performance.now();
  }

  /** Записывает проверку; возвращает `ok`, чтобы удобно ветвить код. */
  check(name, ok, detail) {
    this.checks.push({ name, ok: Boolean(ok), detail: String(detail) });
    this.log(`check ${ok ? 'ok' : 'FAIL'} ${name}: ${detail}`);
    return Boolean(ok);
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
  try {
    await withDeadline(body(context), options.budgetMs ?? PAGE_BUDGET_MS);
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
