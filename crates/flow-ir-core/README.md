# flow-ir-core

flow.ir Pure Rust schema + sync interpreter. The core substrate (layer 2 of the 4-layer flow.ir stack).

No mlua, no async, no I/O — pure schema + `eval` + `Dispatcher` trait.

## Stack position

1. `flow-ir-lua` — Pure Lua DSL (separate repo, ecosystem-neutral)
2. **`flow-ir-core`** — this crate: Pure Rust schema + sync interpreter
3. [`mlua-flow-ir`](https://crates.io/crates/mlua-flow-ir) — async runtime + mlua binding (re-exports this crate)
4. `mlua-swarm-engine` — host concerns (Spawner / Worker / Loop / AuthzPolicy / cp_state persist)

## What's in

- **7 Node kinds** — `Step { ref, in, out }`, `Seq { children }`, `Branch { cond, then, else }`, `Fanout { items, bind, body, join, out }`, `Loop { counter, cond, body, max }`, `Try { body, catch, err_at }`, `Assign { at, value }`
- **20 Expr ops** — read/literal (`Path`, `Lit`), comparison (`Eq`, `Ne`, `Lt`, `Lte`, `Gt`, `Gte`), boolean (`Not`, `And`, `Or`), existence (`Exists`), arithmetic (`Add`, `Sub`, `Mul`, `Div`, `Mod`), aggregate (`Len`, `In`), and the `CallExtern` hatch
- **Discriminated unions** — `#[serde(tag = "kind")]` / `#[serde(tag = "op")]` + `deny_unknown_fields`
- **`Dispatcher` trait** — host provides concrete `dispatch(&str, Value) -> Result<Value>` implementations

## Quick start

```rust
use flow_ir_core::{eval, Dispatcher, EvalError, Node};
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

## License

MIT OR Apache-2.0
