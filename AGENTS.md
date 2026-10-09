# Repository guidance

## Language and compiler

- Treat the semantic analyzer and integration tests as the authority for what
  TypedShell accepts. Oxc can parse syntax that TypedShell intentionally rejects;
  do not document or implement it as supported without adding semantics and
  coverage.
- Keep language rules shared between backends. Put shell-specific output in the
  relevant backend, and preserve the typed IR as the boundary between analysis
  and code generation.
- Generated Bash must remain compatible with Bash 3.2. Generated PowerShell
  targets PowerShell 7.4 or newer. Generated scripts must run without the `tsh`
  executable or a JavaScript runtime installed.
- Preserve the guarantees for ordinary values: quote shell arguments, retain
  argument boundaries, and never evaluate normal strings as shell code. `raw!`
  is the explicit escape hatch and must remain a standalone literal insertion.
- Resolve local imports relative to the importing file, not the process working
  directory. Keep cycle detection, export validation, and inactive conditional
  imports consistent across both targets.
- Keep README and `docs/` claims aligned with implemented behavior and tests.
  State unsupported syntax explicitly instead of implying full TypeScript
  compatibility.

## Working in this repository

- Preserve existing staged and unstaged changes. Do not reset or re-stage the
  user's work as part of an unrelated task.
- The compiler uses Rust 1.97 or newer. Format Rust changes with `cargo fmt`.
- For compiler or backend changes, run `cargo test --all-targets --locked` and
  exercise the relevant examples. When changing import resolution, run
  `cargo run --release -- examples/imports.tsh` from the repository root.
- When changing one backend, check shared compiler behavior and the other
  backend for regressions where the feature is meant to be portable.
