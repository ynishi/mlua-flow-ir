# mlua-flow-ir (workspace)

`flow.ir` IR + interpreter substrate for the `mlua` ecosystem. 3 Node + 3 Expr MVP, no I/O, no concurrency, no agent dispatch — pure substrate.

This workspace publishes two crates:

| Crate | Role |
|---|---|
| [`flow-ir-core`](crates/flow-ir-core) | Pure Rust schema + sync `eval` + `Dispatcher` trait (no mlua, no async) |
| [`mlua-flow-ir`](crates/mlua-flow-ir) | Async runtime (`AsyncDispatcher` / `eval_async` / `fanout_eval`) + mlua `module()` binding. Re-exports `flow-ir-core`. |

Host-side concerns (Spawner / Worker / Loop / AuthzPolicy / cp_state persist) live in the upstream `mlua-swarm-engine` crate. This workspace is intentionally substrate-only.

## Design

- **3 Node kinds** — `Step { ref, in, out }`, `Seq { children }`, `Branch { cond, then, else }` (+ `Fanout` / `Loop` / `Try` in core)
- **3 Expr ops** — `Path { at }`, `Lit { value }`, `Eq { lhs, rhs }`
- **Discriminated** — `#[serde(tag = "kind")]` / `#[serde(tag = "op")]` + `deny_unknown_fields`
- **Open=false** — MVP scope intentionally narrow
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
- **v0.0.4** (current) — workspace split: `flow-ir-core` (Pure Rust) + `mlua-flow-ir` (async + mlua)
- **v0.1** — JSON / YAML loader split into `mlua-flow-json` / `mlua-flow-yaml` sibling crates
- **v0.2+** — Engine integration via `mlua-swarm-engine` (Spawner / Worker / Loop / AuthzPolicy)

## Publish order

`flow-ir-core` first, then `mlua-flow-ir` (depends on `flow-ir-core` via path+version).

## License

MIT OR Apache-2.0
