# Changelog

All notable changes to this workspace (`flow-ir-core` + `mlua-flow-ir`) are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security

## [0.1.1] — 2026-07-05

### Fixed

- `cargo publish` warning "readme `../../README.md` appears to be a path outside of the package" for both crates. `readme` is now declared per-crate as `readme = "README.md"` (resolved relative to the crate directory) instead of inherited from `[workspace.package]`. No behavioural change; packaging metadata only.

## [0.1.0] — 2026-07-05

### Added

- `Expr::CallExtern { ref, args }` — canonical value-shape Hatch. Registered via the new `Externs` trait (Dispatcher-style DI); `ExternMap` for host-side Rust closures; `NoExterns` fallback used by externs-less compat wrappers; `EvalError::ExternError` for unregistered / faulty refs.
- `Expr::Mod` — modulo with canonical Lua `%` semantics (result takes the sign of `rhs`; modulo by zero raises).
- Externs-threaded evaluator entry points: `eval_externs` / `eval_with_storage_externs` / `eval_expr_with_externs` (sync, `flow-ir-core`) and `eval_async_externs` / `eval_async_with_storage_externs` (async, `mlua-flow-ir`). Externs-less APIs (`eval`, `eval_expr`, `eval_async`, ...) remain as compat wrappers that delegate through `NoExterns`.
- `mlua-flow-ir` Lua module: `flow.eval(node, ctx, dispatcher, externs_table?)` — optional 4th arg is a table of pure Lua functions resolved by `call_extern` (canonical `opts.externs` parity, "LuaScript-direct" extension hatch).

### Changed

- **Breaking (wire format)** — `Expr` op tags and field names now match canonical `flow-ir-lua` (`flow/ir/schema.lua`) verbatim: `ge` / `le` become `gte` / `lte`; `and` / `or` use `args` (was `operands`); `not` / `len` / `exists` use `arg` (was `operand` / `of` / `at`). `exists.arg` is now an `Expr` (previously a string path).
- **Breaking (semantics)** — `Exists` now returns `false` for JSON `null` values, mirroring canonical `arg ~= nil` semantics (JSON `null` maps to Lua `nil`). Previously a present-but-null value counted as existing.
- **Breaking (comparison)** — `Lt` / `Lte` / `Gt` / `Gte` now accept string operands (lexicographic byte order, canonical Lua `<` parity) in addition to numbers. Mixed / other types still raise.
- Crate top-level doc updated to reflect the enlarged Node / Expr op surface and the new `Externs` DI layer.

## [0.0.4] — 2026-06-28

### Changed

- **Workspace split**: extracted `flow-ir-core` (Pure Rust schema + sync interpreter, no mlua / no async) and `mlua-flow-ir` (async runtime + mlua binding) into separate crates. `mlua-flow-ir` re-exports the core verbatim so a single import path (`use mlua_flow_ir::*`) keeps working.

### Added

- Per-crate `README.md` and `LICENSE-MIT` / `LICENSE-APACHE`.
- Workspace-level publish metadata (`repository`, `homepage`, `keywords`, `categories`, `documentation`).

## [0.0.3] — pre-split

- mlua bridge full: Lua table → `Node` parse + `flow.eval` Lua entry registered via `module(lua)`.

## [0.0.2] — pre-split

- Async core: `AsyncDispatcher` trait + `eval_async`.

## [0.0.1] — pre-split

- POC: 3 Node + 3 Expr Pure Rust substrate (single crate).
