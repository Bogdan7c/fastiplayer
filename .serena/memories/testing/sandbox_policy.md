# Test Run Sandbox Policy

- Для проекта `<REPO_ROOT>` тестовые прогоны всегда запускать вне sandbox через escalation.
- Это касается `cargo test`, `cargo check`, `cargo clippy`, manual `cargo run`/`xvfb-run` playback scenarios и любых команд проверки, которые используются как validation/test run.
- Если команда проверки уже была запущена в sandbox по ошибке, не считать это финальной validation; повторить нужный прогон вне sandbox с `sandbox_permissions = "require_escalated"`.
- Обычные read-only inspection команды (`rg`, `sed`, `git status`, чтение файлов/логов) могут оставаться в sandbox, если они не являются тестовым прогоном.