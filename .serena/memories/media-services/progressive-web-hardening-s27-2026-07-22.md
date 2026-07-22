# S27 — Progressive/web UX hardening gate (2026-07-22)

## Итог
- Hermetic часть S27 закрыта и прошла полный workspace test gate.
- Production Rust, public/internal API и dependency graph не менялись: milestone закреплён guardrails, документацией и opt-in manual runner-ом.
- Реальный network/GUI прогон не выполнялся, потому что пользователь не передал explicit URL. Его честный статус: `MANUAL REVIEW REQUIRED`, а не автоматический PASS.

## Архитектурные владельцы
- `app-egui::web_media_open` остаётся единственным composition root для yt-dlp web playback.
- `service-ytdlp` владеет extraction/normalization/profile exclusion/neutral request mapping и не может зависеть от concrete HTTP/cache/demux/player owners.
- `web-media-http` владеет progressive HTTP Range/non-Range, redirects, scoped auth/cookies и cancellation/stale fencing.
- PlaylistRuntime/media-open coordinator владеют queue reservation и Ready/authorization/Enqueued/Installed barrier.
- URL sidebar отображает только secret-safe projection и не создаёт второй URL ingress.
- Exact acknowledged locator хранится отдельно от transient `HttpRequestTarget`, `ScopedHttpCookieJar`, `SecretRequestContext`, `TransportOpenRequest` и `YtDlpRequestMaterial`.

## Gate
- `scripts/check-refactor-guardrails.py` проверяет:
  - все startup/preparation/settings yt-dlp ingress используют `crate::web_media_open::prepare_yt_dlp_web_media(...)`;
  - `service-ytdlp` не тянет concrete runtime owners;
  - legacy WebM-only opener/test names отсутствуют;
  - transient transport/auth types не попали в config/playlist/sidebar persistence owners;
  - S27 runner и его self-test подключены.
- `coverage/policy.json` классифицирует `web-media-http` как blocking crate.
- Старые `selected_webm_*` manual aliases удалены из `scripts/media-regression.sh`.
- Playback smoke/schema diagnostics теперь называют актуальную config schema v7.

## Manual runner
- `scripts/progressive-web-smoke.sh` принимает только repeatable explicit `--url` с absolute HTTP(S), обязательный новый `--report`, optional `--duration`, `--binary`, `--dry-run`.
- Нет default corpus, URL discovery, positional URL или file scheme.
- Runner не переопределяет `XDG_CONFIG_HOME`, поэтому production process сохраняет обычный system/user yt-dlp config lookup.
- Raw stdout/stderr живёт только в process-unique temporary directory; report получает только sanitized evidence.
- Sanitizer удаляет exact input URL, любые HTTP(S) endpoint и строки, похожие на transport headers, cookies, request material или extractor payload.
- Report создаётся с owner-only umask, не перезаписывается и всегда требует ручной проверки checklist-а.
- Raw temporary files удаляются обычным EXIT/INT/TERM trap; SIGKILL остаётся общей OS limitation.

## Проверки
- `scripts/ci-checks.sh format-guardrails`: PASS.
- `scripts/ci-checks.sh tests`: PASS для всего workspace и doc-tests.
- Focused web/audio/config/service tests: PASS.
- `cargo +1.96.0 clippy -p service-ytdlp -p web-media-http -p app-egui -p player-core --all-targets --locked -- -D warnings`: PASS.
- Runner self-test доказывает no-args NOT RUN, strict URL parsing, repeated URLs, no overwrite, dry-run и отсутствие URL/header/cookie/extractor secrets в report.

## Связанные memories
- `mem:core`
- `mem:media-services/core`
- `mem:app-egui/media-open-coordinator-s10c`
- `mem:app-egui/sidebar-controller`
