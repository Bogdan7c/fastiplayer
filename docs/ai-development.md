# AI-assisted development

Rustiplayer keeps its owner instructions in [AGENTS.md](../AGENTS.md), project
knowledge in `.serena/memories/`, and optional Codex settings in `.codex/`.
These tools support maintenance; Cargo builds, tests, CI, and running the player
must work without Codex, Context7, Serena, or an AI account. See the
[build instructions](../README.md) and the authoritative
[check runner](../scripts/ci-checks.sh).

## Owner workflow

Read AGENTS.md in full. Its English translation preserves the owner's rules,
including Russian documentation and comments for key production-code logic,
non-obvious decisions and invariants. The owner explicitly clarified during S05
that comments on every line are unnecessary. Explain important choices in plain
language and stop to discuss important decisions before implementing them.

Before a coding task, read Serena `initial_instructions` once per session,
activate the checkout, and read `mem:core` plus relevant memories. Complete
onboarding if necessary. Correct clearly outdated memories before edits. Consult
Context7 before editing or writing code; if relevant documentation is unavailable,
record that limitation and use an official primary source.

Explore code through Serena symbols first. Check references, implementations and
diagnostics before changing an API or ownership boundary. Describe state owners,
boundaries, invariants and functional tests before implementing a feature. Test
observable functionality: playback evidence must reach rendering or the appropriate
audio consumer, not merely prove that a source can be read.

After self-review and relevant checks, update memories when architecture, APIs,
workflow, validation commands, invariants, limitations or key test locations change.
Otherwise state that a memory update was unnecessary. Memory language and technical
depth are not reasons to delete knowledge.

## Serena project identity and portability

The project name in [`.serena/project.yml`](../.serena/project.yml) is `rustiplayer`.
Activate a new checkout by its own directory through `activate_project`; do not
copy another maintainer's absolute path. Once registered in that server, it can
also be activated by name.

A running Serena process may retain the old project name in memory after the YAML
is edited. Restart/reconnect the MCP server and activate the checkout directory
again, then verify `get_current_config` and activation by `rustiplayer`. This is
server state; it does not require changing the name back or committing local
Serena registration files.

Optional local checks, from the repository root with Serena installed:

```sh
serena memories check .
serena project health-check .
```

Read the memory check's output: its CLI always exits successfully even when it
reports stale references. The health check exercises the local language-server
setup and may create ignored cache/log artifacts. Neither command is a build gate.

Public memories use `<REPO_ROOT>` for a checkout, `<MEDIA_DIR>` for media selected
by the person running a test, and `<PRIVATE_BACKUP_DIR>` for an external private
backup location. These are documentation placeholders, not literal shell paths.
Media names describe required properties; they are not an inventory of bundled
files. Follow [manual media regressions](manual-media-regressions.md) and select
inputs explicitly. Preserve codec/container properties, root causes, test names,
measurements and limitations when sanitizing examples.

## Optional maintainer tooling: Codex hooks

[`.codex/hooks.json`](../.codex/hooks.json) retains three commands:

| Event | Command | Purpose |
| --- | --- | --- |
| `SessionStart` (`startup` or `resume`) | `serena-hooks activate --client=codex` | Remind the agent to activate Serena. |
| `PreToolUse` (`Bash`) | `serena-hooks remind --client=codex` | Provide Serena workflow reminders. |
| `Stop` | `serena-hooks cleanup --client=codex` | Clean up the hook's session state. |

These commands require a separately installed `serena-hooks` executable on the
Codex process's `PATH`. Rustiplayer does not install or bundle it. It is optional
maintainer tooling, not a dependency of the player, Cargo, or CI. The executable
is distinct from the Serena MCP connection: having one does not establish the other.

Hooks are **disabled by default** in [`.codex/config.toml`](../.codex/config.toml),
so a fresh clone does not attempt to execute a missing maintainer binary. To opt
in, install and review your chosen compatible Serena hook distribution, verify
`serena-hooks --help` includes `activate`, `remind`, `cleanup` and Codex support,
then launch Codex with an explicit session override:

```sh
codex -c features.hooks=true
```

Use `/hooks` to inspect and trust the project hooks in Codex. Keep installation
paths and account credentials local; do not commit machine-specific commands.
If the binary is absent, leave hooks disabled and follow the Serena workflow
manually. Do not mask a failure of an enabled hook as success. The hook file is
unchanged by this cleanup; no automatic approval hook is configured.

The feature key, project hook format and hook-review flow follow the official
[Codex hooks documentation](https://learn.chatgpt.com/docs/hooks).
Tool availability and integration behavior depend on the installed client/version.

## Optional read-only ChatGPT connection

[`scripts/chatgpt-serena-mcp.sh`](../scripts/chatgpt-serena-mcp.sh) and
[`scripts/chatgpt-serena-tunnel.sh`](../scripts/chatgpt-serena-tunnel.sh) are
maintainer entry points for the existing read-only connection. The former needs
Serena; the latter needs the Secure MCP Tunnel tooling and a local runtime key.
They are not invoked by Cargo or the CI runner. Keep keys outside the repository.
The [read-only context](../.serena/chatgpt-readonly-context.yml) limits exposed tools;
do not turn that connection into a write, shell or project-switching endpoint.
