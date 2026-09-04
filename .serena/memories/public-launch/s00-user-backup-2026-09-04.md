# S00: безопасный backup и вынос `user/` (2026-09-04)

- S00 публичного launch-плана завершён commit `a57a29d28015f2f74e095d1841cabbf39704035e` (`chore: move private user docs outside repository`), parent/pre-S00 HEAD: `c2adea8bcbfa6fbd546231b076c5359790288aee`.
- History rewrite, push, remote-ref mutation и очистка stash/worktrees не выполнялись. После commit `main` был `ahead 1` относительно `origin/main`, рабочее дерево чистое.
- `user/` отсутствует в working tree и index; корневое tracked-правило `/user/` в `.gitignore` запрещает случайное повторное добавление. Будущие сессии не должны возвращать приватные документы под `user/`.
- Приватный backup root: `<PRIVATE_BACKUP_DIR>/rustiplayer-s00-<TIMESTAMP>`, mode `0700`.
  - `repo-user/` — проверенная opaque-копия исходного физического дерева.
  - `repo-user-original/` — атомарно перемещённый исходный каталог; этот recoverable move выбран вместо необратимого удаления, чтобы сохранить явно защищённый untracked `chatgpt_enter_promts.md`.
  - Каталог с пометкой “do not look” не читался и не анализировался; верификация не публикует его содержимое.
- Git-backup: `.../git-backup/rustiplayer-all.bundle`; SHA-256 `98cf7c09f1718d2701fb5adb540422e9c6d03941c1fc3c8ad4b8ac3ed92cece9`.
- Manifest: `.../git-backup/SHA256SUMS`; собственный SHA-256 `7fae708acff504e73024c53fa3cdbfd947331da4a1f1f2675617c41200141c70`. Manifest успешно проверяет 29 Git-backup artifacts; приватное `repo-user` намеренно не разворачивалось в per-file checksum list.
- Bundle verification: `git bundle verify` = complete history; 81 advertised heads = 45 normal refs + 35 registered worktree HEADs + HEAD. Mirror-clone в `/tmp` прошёл `git fsck --full`; normal refs совпали 45/45, все 81 advertised SHA доступны.
- Сохранён stash: `be352ef2e4d8c333e7a6e84d850236a156465cfe refs/stash` / `stash@{0}: On main: pre-rollback-to-e9a0f30`.
- Полные pre-S00 snapshots находятся рядом с bundle: `git-status.txt`, `git-show-ref.txt`, `git-branch-a.txt`, `git-worktree-list.txt`, `git-stash-list.txt`, `head-sha.txt`, remote metadata, bundle verification evidence и ref comparison files.
- Cargo tests не запускались: Rust/code/runtime не менялись. Релевантные gates были recovery verification, checksum verification, exact staged scope, `git diff --cached --check`, empty tracked `user/` и `git check-ignore`.
