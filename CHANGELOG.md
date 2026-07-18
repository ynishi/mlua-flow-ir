# Changelog

All notable changes to this workspace (`flow-ir-core` + `mlua-flow-ir`) are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `[package.metadata.docs.rs]` section on both crates. `flow-ir-core` uses `all-features = true`; `mlua-flow-ir` pins `features = ["lua54", "vendored"]` (mlua's Lua-version features are mutually exclusive at build time, so `all-features = true` would fail). Both add `rustdoc-args = ["--cfg", "docsrs"]` and target `x86_64-unknown-linux-gnu` so docs.rs builds are explicit and reproducible instead of relying on cargo default-feature inference.

### Changed

### Deprecated

### Removed

### Fixed

- Stale `Assign { at, value }` references in per-crate `README.md` and in three comment lines of `crates/mlua-flow-ir/tests/dynamic_injection.rs` (missed during the v0.3.0 rename sweep — the root `README.md` and rustdoc were updated at that time, the per-crate `README.md` and Japanese test comments were not).

### Security

## [0.3.0] — 2026-07-18

### Changed

- **Breaking (Node kind rename)** — `Node::Assign` is now `Node::Let`, matching the canonical `flow-ir-lua` schema (`flow/ir/schema.lua` `§Node.let`). The wire tag changes from `"assign"` to `"let"`, and the `at` field is downgraded from `Expr` (a `Path`-wrapping `Expr::Path`) to a bare `Path`, serialized as a plain path string. Wire format before: `{"kind": "assign", "at": {"op": "path", "at": "$.foo"}, "value": ...}` → after: `{"kind": "let", "at": "ctx.foo", "value": ...}`. Rust API: `Node::Assign { at: Expr, value: Expr }` → `Node::Let { at: Path, value: Expr }`. The v0.2.x `"kind": "assign"` tag is no longer accepted and the nested-`Expr` `at` shape is rejected at deserialize time.
- **Breaking (`Path` parser root-token whitelist)** — the parser now accepts both `$` (canonical read prefix) and `ctx` (canonical write prefix, used by `Node::Let.at`) as leading root tokens. Bracket forms (`ctx.a["p.md"]` / `ctx["x.y"]`) work identically for either root. Root-token distinction (read vs. write) is delegated to the caller (the surrounding `Node` field contract) rather than encoded in the parser. The v0.2.0 typo-suspender rejections are preserved: `$foo`, `ctxfoo`, `foo.bar`, empty segments, malformed brackets are all still rejected up front. `Path`'s `Display` impl round-trips the original root token verbatim.
- **Breaking (MSRV)** — `rust-version` bumped from `1.77` to `1.85`. Required by the transitive `lua-src v550.0.0` dependency (pulled in via `mlua`'s `vendored` feature), which requires `edition2024` (Cargo ≥ `1.85`). The v0.2.0 CI `msrv (1.77)` job failed on `main` after `lua-src` released `v550.0.0`; the CI job is now `msrv (1.85)` and green.

## [0.2.0] — 2026-07-11

### Added

- `flow_ir_core::Path` — a typed, parsed context path (parse-don't-validate). `Expr::Path.at` (and every other path-carrying field, via the same `Expr::Path`) is now a `Path`, deserialize-time-validated, rather than a raw `String`. `PathParseError` is the dedicated parse-error type surfaced through `Path`'s `Deserialize` impl and through the `read_path`/`write_path` `&str` compat wrappers (as `EvalError::InvalidPath`).
- `EvalError::TypeError { op, msg }` — an expression/node received a value of the wrong type.
- `EvalError::ArithError { op, msg }` — an arithmetic operation failed (division/modulo by zero, or a numeric value that cannot be represented as `f64`).
- `mlua-flow-ir` feature flags — `mlua` is now an optional dependency with Lua-version passthrough features (`lua51`/`lua52`/`lua53`/`lua54`/`luajit`/`luau`) and a weak `vendored` feature. `default = ["lua54", "vendored"]` keeps existing consumers unchanged; `default-features = false` gives an async-only build (no Lua binding, no vendored C compilation).
- CI (GitHub Actions): fmt / clippy `-D warnings` / tests / rustdoc `-D missing_docs` gates plus an MSRV (1.77) check and an async-only (`--no-default-features`) build check.

### Changed

- **Breaking (path syntax)** — malformed path syntax is now rejected uniformly, at parse time (`Path::from_str`, exercised at deserialize time for `Expr::Path.at` and friends): a path not starting with `$` followed immediately by `.`, `[`, or end-of-string is rejected (`$foo` used to be silently accepted as a 1-segment dot path); any empty dot segment (`$.`, `$.a.`, `$.a..b`) is rejected (previously silently dropped on write, or surfaced as `PathNotFound` rather than `InvalidPath` on read, depending on whether the ctx happened to have a key `""` at that position).
- **Breaking (write semantics)** — writing through a path whose intermediate segment already holds a concrete non-object value (string, number, bool, array) now raises `EvalError::TypeError` instead of silently clobbering it with a fresh object. `null`/absent intermediates still auto-promote to an empty object, unchanged.
- **Breaking (Fanout empty items)** — `Any`/`Race` with an empty `items` array now raise `EvalError::TypeError` (Promise.any/Promise.race parity: zero items can never produce a winner) instead of returning `Value::Array([])`. `All`/`AllSettled` are unchanged (still return `[]`, consistent with their array-shaped result). Sync (`flow-ir-core`) and async (`mlua-flow-ir`) evaluators are in lockstep on this.
- **Breaking (`EvalError`)** — the enum is now `#[non_exhaustive]`; match on it with a wildcard arm. `DispatcherError`/`ExternError` are now raised exclusively for real `Dispatcher`/`Externs` failures — every synthetic-`ref_` internal-evaluator error (`"expr.len"`, `"expr.in"`, `"expr.cmp"`, `"expr.{op}"` arithmetic ops, `"fanout.items"`) has been rerouted to `TypeError` or `ArithError`, matching the failure's actual nature (wrong type vs. arithmetic failure).
- `read_path`/`write_path` keep their existing `&str`-in signatures as thin wrappers over `Path`; internal `Node`/`Expr` evaluation now walks an already-parsed `Path` directly rather than re-parsing a path string on every read/write.
- **Breaking (numeric equality)** — `Eq`/`Ne`/`In` now compare numbers by value with Lua parity (`5 == 5.0` is true), matching the f64 coercion `Lt`/`Lte`/`Gt`/`Gte` already used. Previously `serde_json::Value`'s `PartialEq` distinguished integer and float representations, so `eq(add(2,3), lit(5))` evaluated to `false` (arithmetic ops always emit floats). Integers above 2^53 share the same precision caveat as the ordering ops.

### Fixed

- crates.io README for `mlua-flow-ir`: the async example implemented a nonexistent `dispatch_async` method (the trait method is `dispatch`).
- Stale README claims: "3 Node + 3 Expr" (actual: 7 Node kinds + 20 Expr ops) and a roadmap stuck at "v0.0.4 (current)".
- Missing rustdoc: `#![warn(missing_docs)]` is now enforced on both crates and every public item is documented.

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
