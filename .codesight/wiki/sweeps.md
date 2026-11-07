# Drift sweeps

Checks that keep what we publish true to what the code does (epic #1720). Each lane owns its own data file in `scripts/` or its test; this article has one section per lane.

## CLI invocations (#1721)

- **Parse check, every PR:** `verifiable-log-serve/tests/doc_invocations.rs` extracts every `pollis-verify …` command from tracked docs, website pages, `/learn`, READMEs and workflows, and parses each with the shipped clap definition (`verifiable_log_serve::cli`). Only code counts as a command: fenced blocks and inline code in Markdown, `<code>`/`<pre>` in HTML (and HTML inside JS strings), and whole script/workflow files; prose naming the tool is skipped. `<placeholders>`, `$VARS`, `{x}` and `vX.Y.Z` parse as dummies. A subcommand named alone (or with only its flags, `release --json`) is a reference and only has to exist. It runs in `.github/workflows/doc-invocations.yml`, which triggers on doc-only changes.
- **Reverse check:** every subcommand and every long flag, plus `--version`, must appear in a public doc (`website/`, `docs/verify-transparency-log.md`, `docs/transparency.md`, `README.md`). A new subcommand or flag without docs fails.
- **Live check, daily:** commands with no placeholders that target `https://verify.pollis.com` are listed in `verifiable-log-serve/tests/doc_invocations.live.txt` (the test fails if the list is stale; regenerate with `UPDATE_DOC_INVOCATIONS=1 cargo test -p verifiable-log-serve --test doc_invocations`). `website-verify.yml` downloads the latest `pollis-verify-v*` release, checks its `.sha256`, and runs each one against the live log; any non-zero exit fails.
- **`pollis` (the terminal client):** not covered. It has no clap definition and no subcommands, only `--help` and `--version` (`pollis-tui/src/cli.rs`, whose own tests pin the help text), so there is no invocation surface to drift.
- **Installers:** `scripts/check-install-env.py` (in `scripts-check.yml`) checks that every `POLLIS_*` variable `website/install.sh` / `cli-install.sh` reads is documented (today in `docs/deployments.md`), and that no doc shows one with an installer that ignores it.
