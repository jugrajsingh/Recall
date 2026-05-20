# Recall (jugrajsingh fork)

> Local-first search across every AI coding session on your machine.

> **This is a fork** of [samzong/Recall](https://github.com/samzong/Recall) with significant local additions. See [Fork divergence](#fork-divergence-from-samzongrecall) for what's new. The original README sections below describe the base capabilities; the fork-specific sections sit on top.

[![Recall TUI](recall.png)](https://asciinema.org/a/909453)

You bounce between Claude Code, Codex, Copilot CLI, and whatever comes next. Each tool keeps its own sessions in its own place, in its own format. Recall pulls them all into one local index you can actually search — and drops you right back into any session in its original CLI.

## Fork divergence from `samzong/Recall`

This fork (`feature/devtoolkit-fork` branch) carries the following additions on top of upstream v0.1.6. None of them are upstream yet; rebasing onto upstream will conflict in the files listed.

### Build variants — pick your weight class

The original recall is one binary with candle + hf-hub + tokenizers + system openssl. This fork adds Cargo features so you can pick.

| Variant | How to build | Binary size | Cold build time | Semantic? | TLS chain |
|---|---|---|---|---|---|
| **`recall-mini`** | `cargo build --release --no-default-features` | ~12 MB | ~1 min | FTS5 only | none — no openssl |
| **`recall-full` (fastembed, default)** | `cargo build --release` | ~22 MB | ~2 min | ✅ hybrid FTS5 + semantic via ONNX | rustls (pure Rust) |
| **`recall-full` (candle)** | `cargo build --release --no-default-features --features semantic-search,semantic-candle` | ~30 MB | ~3 min | ✅ hybrid + Metal/CUDA acceleration | native-tls (system openssl) |

`make release-mini` / `release-full` install to `~/.cargo/bin/recall`. `recall info` reports which variant you're running.

### New / changed CLI subcommands

- `recall prune [--dry-run]` — delete DB rows that are no longer eligible (file gone, source disabled, matches an exclusion glob). Run after editing config; then `recall sync` to pick up new files.
- `recall reset [--yes]` — wipe `~/.local/share/recall/recall.db` cleanly. Survives corrupted FTS / sqlite-vec virtual-table state. Stops the worker first.
- `recall vacuum` — `VACUUM` + `ANALYZE` with a before/after byte-count report.
- `recall worker {status, stop [--clear-queue]}` — inspect the background semantic worker, kill it cleanly (SIGTERM), or drop the queue entirely.
- `recall config {show, edit, doctor}` — print the resolved config, open it in `$EDITOR` with on-save validation, or diagnose perms/glob/feature-flag issues.
- `recall reembed [--yes]` — drop vector embeddings and re-enqueue everything (full build only). Use after switching backend.

### New config option: `excluded_paths`

`~/.config/recall/config.json` now supports a `excluded_paths: [...]` array of globs. Sessions whose `cwd` OR on-disk JSONL file path matches any glob are dropped at sync time. Useful for excluding claude-mem observer / sub-agent sessions that would otherwise pollute the index.

```json
{
  "excluded_paths": [
    "**/.claude-mem/observer-sessions/**",
    "**/*claude-mem-observer-sessions*/**"
  ]
}
```

### Better session labels from JSONL

The Claude Code adapter now reads three additional event types from each transcript (ported from [raine/claude-history](https://github.com/raine/claude-history)):

- `customTitle` — what `/rename` writes inside Claude. Shown as the row label when present.
- `summary` — Claude's auto-generated session summary. Used as the label fallback before deriving a slug from the first user message.
- `duration_minutes` — computed from the first→last message timestamp. Shown next to msg count.

The decoder that mangled `~/.claude-mem/observer-sessions` into `~/.claude/mem/observer/sessions` is removed; the JSONL `cwd` field is now authoritative. Machinery messages (`isCompactSummary`, `isSidechain`, `isMeta`) are filtered at parse time so resumed sessions stop title-colliding on the auto-injected "This session is being continued..." block.

### TUI changes

- **Two-line list rows** — header (project · label) and dimmed snippet, with cluster-dedup `×N` badge when multiple sessions share `(cwd, first message)`.
- **Hybrid timestamp** — relative ("3h", "5d") for recent, calendar ("Mar 14") for older than 14 days.
- **Viewport scroll fix** — content pre-wrapped via `textwrap` so the selected message stays in view at any list length (the upstream's `chars / width` estimate drifts at 1000+ messages).
- **`ListState`-driven** session list — selection always visible, like k9s / htop.
- **Panel-aware mouse routing** — scroll and click route to whichever panel the cursor is over, not the active one.
- **Mouse capture default OFF** — drag-select text immediately, like in `claude-history`. `Ctrl+M` re-enables click/scroll panel mode.
- **Tab toggles panel focus** (was: cycle filters); filter values live exclusively in the `Ctrl+S` settings panel which now shows all options as chips.
- `q` quit removed from search mode so queries starting with `q` don't exit the app.
- `Ctrl+E` toggles per-message expand/collapse (collapsed messages show `(K more lines)` hint).

### CVE remediation

Bumped openssl, rustls-webpki, rand to clear 6 HIGH-severity CVEs in upstream's lock file (see `Cargo.lock` history). The fastembed default fully avoids openssl going forward.

### Repo hygiene

- `.pre-commit-config.yaml` — formatting + secret scan (gitleaks). Run `pre-commit install` once.
- `Makefile` — `release-mini`, `release-full`, `install-mini`, `install-full` targets so the dev loop never leaves the repo dir.

## Architecture

![Recall Architecture](docs/architecture.png)

## Install

```bash
brew install samzong/tap/recall
# or
make install # clone
```

## Support

One index across every AI coding CLI. Sync once, search everywhere, resume right where you left off.

| Capability             | Claude Code | OpenCode | Codex | Antigravity CLI | Gemini | Kiro | Copilot CLI | Cursor |
| ---------------------- | :---------: | :------: | :---: | :-------------: | :----: | :--: | :---------: | :----: |
| Auto-discovery         |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| Full index             |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| Incremental sync       |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| FTS5 keyword search    |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| Semantic search        |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| Source filter          |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| Time range filter      |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| In-session search      |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| Copy message           |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| Export to Markdown     |     ✅      |    ✅    |  ✅   |       ✅        |   ✅   |  ✅  |     ✅      |   ✅   |
| Resume in original CLI |     ✅      |    ✅    |  ✅   |       ✅        |   —    |  —   |     ✅      |   —    |

## Usage

```bash
recall sync          # incremental sync (safe to run anytime)
recall sync --force  # reprocess every session (after changing embedding model)
recall               # launch TUI
recall search Q      # one-shot CLI search
recall info          # index stats and worker status
```

## License

[MIT](LICENSE)
