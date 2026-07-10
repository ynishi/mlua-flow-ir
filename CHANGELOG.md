# Changelog

All notable changes to this workspace (`flow-ir-core` + `mlua-flow-ir`) are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `flow_ir_core::Path` — a typed, parsed context path (parse-don't-validate). `Expr::Path.at` (and every other path-carrying field, via the same `Expr::Path`) is now a `Path`, deserialize-time-validated, rather than a raw `String`. `PathParseError` is the dedicated parse-error type surfaced through `Path`'s `Deserialize` impl and through the `read_path`/`write_path` `&str` compat wrappers (as `EvalError::InvalidPath`).
- `EvalError::TypeError { op, msg }` — an expression/node received a value of the wrong type.
- `EvalError::ArithError { op, msg }` — an arithmetic operation failed (division/modulo by zero, or a numeric value that cannot be represented as `f64`).

### Changed

- **Breaking (path syntax)** — malformed path syntax is now rejected uniformly, at parse time (`Path::from_str`, exercised at deserialize time for `Expr::Path.at` and friends): a path not starting with `$` followed immediately by `.`, `[`, or end-of-string is rejected (`$foo` used to be silently accepted as a 1-segment dot path); any empty dot segment (`$.`, `$.a.`, `$.a..b`) is rejected (previously silently dropped on write, or surfaced as `PathNotFound` rather than `InvalidPath` on read, depending on whether the ctx happened to have a key `""` at that position).
- **Breaking (write semantics)** — writing through a path whose intermediate segment already holds a concrete non-object value (string, number, bool, array) now raises `EvalError::TypeError` instead of silently clobbering it with a fresh object. `null`/absent intermediates still auto-promote to an empty object, unchanged.
- **Breaking (Fanout empty items)** — `Any`/`Race` with an empty `items` array now raise `EvalError::TypeError` (Promise.any/Promise.race parity: zero items can never produce a winner) instead of returning `Value::Array([])`. `All`/`AllSettled` are unchanged (still return `[]`, consistent with their array-shaped result). Sync (`flow-ir-core`) and async (`mlua-flow-ir`) evaluators are in lockstep on this.
- **Breaking (`EvalError`)** — the enum is now `#[non_exhaustive]`; match on it with a wildcard arm. `DispatcherError`/`ExternError` are now raised exclusively for real `Dispatcher`/`Externs` failures — every synthetic-`ref_` internal-evaluator error (`"expr.len"`, `"expr.in"`, `"expr.cmp"`, `"expr.{op}"` arithmetic ops, `"fanout.items"`) has been rerouted to `TypeError` or `ArithError`, matching the failure's actual nature (wrong type vs. arithmetic failure).
- `read_path`/`write_path` keep their existing `&str`-in signatures as thin wrappers over `Path`; internal `Node`/`Expr` evaluation now walks an already-parsed `Path` directly rather than re-parsing a path string on every read/write.

### Deprecated

### Removed

### Fixed

### Security

## [0.1.2] — 2026-07-10

### Added

- Bracket notation for keys containing dots in `read_path` / `write_path`, RFC 9535 (JSONPath) style: `$.a["plan.md"]`, `$["x.y"]` (#1).

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
