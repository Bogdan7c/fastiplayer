# yt-dlp external-process ownership (2026-08-05)

- Общая internal boundary находится в `crates/service-ytdlp/src/process_tree.rs`. Оба process caller-а — candidate/recovery `src/process.rs` и topology `src/topology/process.rs` — не владеют `std::process::Child` напрямую.
- `spawn_owned_process` на Unix создаёт отдельную process group (PGID = PID root child) и возвращает только `OwnedProcess`. На non-Unix сохраняется fallback одного child.
- Unix spawn имеет bounded operational recovery только для `ETXTBSY`: максимум 8 попыток с паузой не более 10 ms. Между попытками проверяется cancellation; retry не запускается после исходного process deadline; exhaustion/deadline сохраняет последний OS error как прежний typed `ProcessFailure`. После успешного spawn wait использует только остаток исходного timeout.
- `OwnedProcess` — единственный владелец child/group. Caller получает только intent methods `take_stdout`, `take_stderr`, `try_wait`, `finish`. `finish` всегда завершает всю owned group и reap-ит root, в том числе после нормального root exit: lingering descendants могут удерживать унаследованные stdout/stderr pipe-ы.
- Обязательный порядок lifecycle: setup/wait завершается -> `finish` group+root -> join обоих pipe readers -> mapping результата. Поэтому missing pipe, reader spawn failure, try_wait failure, timeout, cancellation, topology budget overflow, normal exit и ранний return не оставляют child/zombie/descendant и не блокируют join.
- `Drop for OwnedProcess` — аварийная RAII-страховка для panic/непредвиденного раннего выхода. Она вызывает тот же kill+reap; cleanup failure не игнорируется молча и печатается как safe OS-only diagnostic.
- При успешной cleanup сохраняются исходные typed `Timeout`, `Cancellation` и topology budget errors. Если primary failure совпал с cleanup/join failure, обе причины сохраняются структурно в internal `OwnedProcessCleanupFailure`, который сверху остаётся secret-safe `ProcessFailure`; raw locator/argv/stderr не добавляются.
- Unix dependency `libc` подключена target-specific в `crates/service-ytdlp/Cargo.toml` для group signal и `ETXTBSY`.
- Ключевые functional regressions:
  - `process_tree::tests::setup_abort_drop_kills_descendant_and_unblocks_pipe_reader`: после реального descendant `sleep 30` setup abort через Drop быстро закрывает pipe.
  - `process::tests::process_normal_root_exit_does_not_wait_for_lingering_descendant`: успешный root status/stdout сохраняется, lingering `sleep 30` не задерживает возврат.
  - `process::tests::process_spawn_*`: transient success, exhaustion, cancellation и запрет retry после deadline.
  - `process::tests::cancelled_recovery_cleans_working_directory`: cancellation group cleanup не ждёт descendant.
- Focused verification: `cargo +1.96.0 test -p service-ytdlp --locked process::tests`, `cargo +1.96.0 test -p service-ytdlp --locked process_tree::tests`, strict `cargo +1.96.0 clippy -p service-ytdlp --all-targets --locked -- -D warnings`, rustfmt, diff-check и Serena diagnostics.

## P1 lifecycle hardening (2026-08-08)

- На Unix `poll_root_exit` использует `waitid(P_PID, ..., WEXITED | WNOHANG | WNOWAIT)`: завершившийся root остаётся waitable до group termination и `Child::wait`, поэтому его PID/PGID не может быть переиспользован между наблюдением exit и сигналом owned process group. `EINTR` повторяется, неожиданный PID считается typed process failure.
- Если group signal падает, а root уже удалось reap-нуть, `OwnedProcessTerminationFailure` сохраняет и primary OS error, и `ExitStatus`; повторный отрицательный kill по уже освобождённому PGID запрещён.
- Pipe readers на Unix переводятся в `O_NONBLOCK` и принадлежат `OwnedPipeReader`. Чтение stop-aware; private sentinel использует non-retryable `ConnectionAborted` (не `Interrupted`, который `read_to_end` автоматически повторяет). Worker закрывает reader/FD до публикации completion; Drop reader-owner выставляет stop как panic/early-return fallback.
- Drain ограничен меньшим из остатка исходного operation deadline и 500 ms; abort также bounded 500 ms. Descendant, ушедший через `setsid` и удерживающий pipe, не маскируется как success и не создаёт вечный join: возвращается typed `ProcessFailure`. Boundary владеет process group, а не произвольным reparented process tree.
- Дополнительные functional regressions: WNOWAIT сохраняет waitability и exit code; normal root exit с same-PGID descendant быстро завершается; escaped-PGID pipe holder даёт bounded failure; explicit reader abort подтверждает завершение worker и закрытие FD.
- Проверено: `process_tree::tests` 3/3, `process::tests` 19/19, полный `service-ytdlp` green, strict Clippy, workspace `hermetic-ci` PASS и release build PASS.

Related: `mem:core`, `mem:media-services/core`, `mem:media-services/ytdlp-topology-s15-2026-07-20`.