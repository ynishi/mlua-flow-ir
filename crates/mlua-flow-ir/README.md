# mlua-flow-ir

flow.ir async runtime + mlua binding. Layer 3 of the 4-layer flow.ir stack.

Re-exports the Pure Rust core ([`flow-ir-core`](https://crates.io/crates/flow-ir-core)) and adds `AsyncDispatcher` + `eval_async` (including `Fanout` join-mode support) + Lua `module()` binding.

## Stack position

1. `flow-ir-lua` — Pure Lua DSL (separate repo, ecosystem-neutral)
2. [`flow-ir-core`](https://crates.io/crates/flow-ir-core) — Pure Rust schema + sync interpreter
3. **`mlua-flow-ir`** — this crate: async runtime + mlua binding
4. `mlua-swarm-engine` — host concerns (Spawner / Worker / Loop / AuthzPolicy / cp_state persist)

## What's in

- All `flow-ir-core` schema types re-exported verbatim (`Node` / `Expr` / `Dispatcher` / `EvalError` / …)
- `AsyncDispatcher` trait + `eval_async` for tokio / async-runtime hosts
- `Fanout` parallel `Step` dispatch (`All` / `Any` / `Race` / `AllSettled` join modes) via `eval_async`
- `module(lua)` binding that registers `flow.eval` into a Lua state

## Quick start (sync)

```rust
use mlua_flow_ir::{eval, Dispatcher, EvalError, Node};
use serde_json::{json, Value};

let node: Node = serde_json::from_value(json!({
    "kind": "step",
    "ref": "uppercase",
    "in": { "op": "path", "at": "$.input" },
    "out": { "op": "path", "at": "$.output" },
})).unwrap();

struct Fixture;
impl Dispatcher for Fixture {
    fn dispatch(&self, _r: &str, input: Value) -> Result<Value, EvalError> {
        if let Value::String(s) = input {
            Ok(Value::String(s.to_uppercase()))
        } else {
            Ok(input)
        }
    }
}

let result = eval(&node, json!({ "input": "hello" }), &Fixture).unwrap();
assert_eq!(result, json!({ "input": "hello", "output": "HELLO" }));
```

## Async dispatch

```rust
use mlua_flow_ir::{eval_async, AsyncDispatcher, EvalError, Node};
use async_trait::async_trait;
use serde_json::Value;

struct AsyncFixture;

#[async_trait]
impl AsyncDispatcher for AsyncFixture {
    async fn dispatch(&self, _r: &str, input: Value) -> Result<Value, EvalError> {
        Ok(input)
    }
}
```

## License

MIT OR Apache-2.0
