// Классический Dedicated Worker повтора записи (контекст `worker-N`). Страница создаёт его из
// blob-адреса с `importScripts('<адрес этого файла>')` — так делает Twitch (IVS); движок
// подгружается динамическим `import()` по абсолютному адресу из сообщения `run`.
//   page -> worker: run {coreUrl, contextName, events, store (transfer), t0Epoch}
//   worker -> page: handle {ms, handle (transfer)}, log {line}, done {result} | failed {message}

self.addEventListener('message', async (event) => {
  const message = event.data;
  if (message.type !== 'run') return;
  try {
    const { runContext } = await import(message.coreUrl);
    const host = {
      log: (line) => self.postMessage({ type: 'log', line }),
      special: {
        // `getHandle` в воркере: handle уезжает странице, она привяжет его к <video>.
        media_source_handle: (recorded, state) => {
          const mediaSource = state.mediaSources.get(recorded.ms);
          if (!mediaSource) throw new Error(`no MediaSource ${recorded.ms}`);
          const handle = mediaSource.handle;
          self.postMessage({ type: 'handle', ms: recorded.ms, handle }, [handle]);
        },
      },
    };
    const result = await runContext({
      contextName: message.contextName,
      events: message.events,
      store: message.store,
      t0Epoch: message.t0Epoch,
      host,
    });
    self.postMessage({ type: 'done', result });
  } catch (error) {
    self.postMessage({ type: 'failed', message: `${error?.name}: ${error?.message}` });
  }
});
