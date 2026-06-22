# graphify reference: commit hook and native [removed-harness-document] integration

Load this when the user asked to install the post-commit hook or wire graphify into a project's [removed-harness-document].

## For git commit hook

Install a post-commit hook that auto-rebuilds the graph after every commit. No background process needed - triggers once per commit, works with any editor.

```bash
graphify hook install    # install
graphify hook uninstall  # remove
graphify hook status     # check
```

After every `git commit`, the hook detects which code files changed (via `git diff HEAD~1`), re-runs AST extraction on those files, and rebuilds `graph.json` and `GRAPH_REPORT.md`. Doc/image changes are ignored by the hook - run `/graphify --update` manually for those.

If a post-commit hook already exists, graphify appends to it rather than replacing it.

---

## For native [removed-harness-document] integration

Run once per project to make graphify always-on in [removed non-Codex harness] sessions:

```bash
graphify removed-harness install
```

This writes a `## graphify` section to the local `[removed-harness-document]` that instructs [removed non-Codex harness] to check the graph before answering codebase questions and rebuild it after code changes. No manual `/graphify` needed in future sessions.

```bash
graphify removed-harness uninstall  # remove the section
```
