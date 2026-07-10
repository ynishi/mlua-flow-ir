# mlua-flow-ir (workspace)

`flow.ir` IR + interpreter substrate for the `mlua` ecosystem. 7 Node kinds + 20 Expr ops, no I/O, no concurrency, no agent dispatch — pure substrate.

This workspace publishes two crates:

| Crate | Role |
|---|---|
| [`flow-ir-core`](crates/flow-ir-core) | Pure Rust schema + sync `eval` + `Dispatcher` trait (no mlua, no async) |
| [`mlua-flow-ir`](crates/mlua-flow-ir) | Async runtime (`AsyncDispatcher` / `eval_async`, including `Fanout` join-mode support) + mlua `module()` binding. Re-exports `flow-ir-core`. |

Host-side concerns (Spawner / Worker / Loop / AuthzPolicy / cp_state persist) live in the upstream `mlua-swarm-engine` crate. This workspace is intentionally substrate-only.

## Design

- **7 Node kinds** — `Step { ref, in, out }`, `Seq { children }`, `Branch { cond, then, else }`, `Fanout { items, bind, body, join, out }`, `Loop { counter, cond, body, max }`, `Try { body, catch, err_at }`, `Assign { at, value }`
- **20 Expr ops** — read/literal (`Path`, `Lit`), comparison (`Eq`, `Ne`, `Lt`, `Lte`, `Gt`, `Gte`), boolean (`Not`, `And`, `Or`), existence (`Exists`), arithmetic (`Add`, `Sub`, `Mul`, `Div`, `Mod`), aggregate (`Len`, `In`), and the `CallExtern` hatch
- **Discriminated** — `#[serde(tag = "kind")]` / `#[serde(tag = "op")]` + `deny_unknown_fields`
- **Dispatcher = callback** — host provides concrete implementations (process spawn, mlua callback, MCP call, direct LLM, etc.)

## Quick start

```rust
use mlua_flow_ir::{eval, Dispatcher, EvalError, Node};
use serde_json::{json, Value};

let node: Node = serde_json::from_value(json!({
    "kind": "step",
    "ref": "uppercase",
    "in": { "op": "path", "at": "$.input" },
    "out": { "op": "path", "at": "$.output" },
})).unwrap();

struct FixtureDispatcher;
impl Dispatcher for FixtureDispatcher {
    fn dispatch(&self, _ref: &str, input: Value) -> Result<Value, EvalError> {
        if let Value::String(s) = input {
            Ok(Value::String(s.to_uppercase()))
        } else {
            Ok(input)
        }
    }
}

let result = eval(&node, json!({ "input": "hello" }), &FixtureDispatcher).unwrap();
assert_eq!(result, json!({ "input": "hello", "output": "HELLO" }));
```

## Roadmap

- **v0.0.1–0.0.3** — pre-split prototype (single crate)
- **v0.0.4** — workspace split: `flow-ir-core` (Pure Rust) + `mlua-flow-ir` (async + mlua)
- **v0.1.x** (current) — `Expr::CallExtern` hatch + `Externs` DI registry, `Expr::Mod`, canonical wire-format alignment (`gte`/`lte`, `args`/`arg`), `Node::Loop` / `Node::Try` / `Node::Assign`, RFC 9535-style bracket path notation. See [CHANGELOG.md](CHANGELOG.md) for the full per-release list.
- **Future** — JSON / YAML loader split into `mlua-flow-json` / `mlua-flow-yaml` sibling crates; engine integration via `mlua-swarm-engine` (Spawner / Worker / Loop / AuthzPolicy)

## Publish order

`flow-ir-core` first, then `mlua-flow-ir` (depends on `flow-ir-core` via path+version).

## License

MIT OR Apache-2.0
