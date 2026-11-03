// Проба «воркер из http(s)-адреса того же сайта экстеншен НЕ оборачивает» (решение 192).
// Обычный сайт создаёт так свои воркеры (не MSE): обёртка сменила бы им базовый адрес на blob, и
// относительные `fetch`/`importScripts`/`new URL(x, self.location)` сломались бы. Проба докладывает
// странице свой `self.location.href` и исход относительного `fetch` (этого же файла).
(async () => {
  let fetchOk = false;
  let detail = '';
  try {
    const response = await fetch('./same-origin-probe-worker.js', { cache: 'no-store' });
    fetchOk = response.ok;
    detail = `status=${response.status}`;
  } catch (error) {
    detail = `fetch failed: ${error}`;
  }
  self.postMessage({ href: self.location.href, fetchOk, detail });
})();
