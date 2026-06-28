# Changelog

All notable changes to this workspace (`flow-ir-core` + `mlua-flow-ir`) are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
