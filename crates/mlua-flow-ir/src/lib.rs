#![deny(unsafe_code)]
//! flow.ir async runtime + mlua binding.
//!
//! Layer 3 of the 4-layer flow.ir stack:
//!
//! 1. `flow-ir-lua` — Pure Lua DSL (separate repo, ecosystem-neutral)
//! 2. `flow-ir-core` — Pure Rust schema + sync interpreter (no mlua, no async)
//! 3. `mlua-flow-ir` — **this crate**: re-export of `flow-ir-core` +
//!    `AsyncDispatcher` + `eval_async` + `fanout_eval` + Lua `module()` binding
//! 4. `mlua-swarm-engine` — host concerns (Spawner / Worker / Loop /
//!    AuthzPolicy / cp_state persist)
//!
//! All schema types (`Node` / `Expr` / `JoinMode` / `EvalError` / `Dispatcher`)
//! are re-exported verbatim from `flow-ir-core` so callers can keep a single
//! import path:
//!
//! ```
//! use mlua_flow_ir::{eval, eval_async, AsyncDispatcher, Dispatcher, EvalError, Expr, Node};
//! ```

// ──────────────────────────────────────────────────────────────────────────
// Re-export Pure Rust core (flow-ir-core)
// ──────────────────────────────────────────────────────────────────────────

pub use flow_ir_core::{
    eval, eval_expr, is_truthy, read_path, write_path, Dispatcher, EvalError, Expr, JoinMode, Node,
};

use serde_json::Value;

// ══════════════════════════════════════════════════════════════════════════
// v0.0.2 — Async core (eval_async + AsyncDispatcher trait)
// ══════════════════════════════════════════════════════════════════════════

use async_recursion::async_recursion;
use async_trait::async_trait;

/// Async dispatcher trait — async 版 `Dispatcher`。
///
/// `async_trait` macro 経由 (= Rust 2021 互換 + dyn safe)。 Host crate
/// (e.g. mlua-swarm-engine `AsyncSpawner`) が impl する。 substrate には
/// tokio dep 入れない (= Pure 維持)、 executor は caller (host) 責務。
#[async_trait]
pub trait AsyncDispatcher: Send + Sync {
    async fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError>;
}

/// Evaluate a `Node` against a context value asynchronously,
/// using the given async dispatcher for `Step` resolution.
///
/// `eval` (sync) と同型 logic、 dispatch を `.await` に置き換え。 Seq / Branch
/// は recursive async fn (= `async_recursion` macro で `Pin<Box>` wrap)。
///
/// # Quick start
///
/// ```
/// use async_trait::async_trait;
/// use mlua_flow_ir::{eval_async, AsyncDispatcher, EvalError, Expr, Node};
/// use serde_json::{json, Value};
///
/// struct Fixture;
///
/// #[async_trait]
/// impl AsyncDispatcher for Fixture {
///     async fn dispatch(&self, _r: &str, input: Value) -> Result<Value, EvalError> {
///         if let Value::String(s) = input {
///             Ok(Value::String(s.to_uppercase()))
///         } else {
///             Ok(input)
///         }
///     }
/// }
///
/// let rt = tokio::runtime::Runtime::new().unwrap();
/// rt.block_on(async {
///     let node = Node::Step {
///         ref_: "up".into(),
///         in_: Expr::Path { at: "$.input".into() },
///         out: Expr::Path { at: "$.output".into() },
///     };
///     let out = eval_async(&node, json!({ "input": "hello" }), &Fixture).await.unwrap();
///     assert_eq!(out, json!({ "input": "hello", "output": "HELLO" }));
/// });
/// ```
#[async_recursion]
pub async fn eval_async<D>(node: &Node, ctx: Value, dispatcher: &D) -> Result<Value, EvalError>
where
    D: AsyncDispatcher + ?Sized,
{
    match node {
        Node::Step { ref_, in_, out } => {
            let input = eval_expr(in_, &ctx)?;
            let output =
                dispatcher
                    .dispatch(ref_, input)
                    .await
                    .map_err(|e| EvalError::DispatcherError {
                        ref_: ref_.clone(),
                        msg: e.to_string(),
                    })?;
            write_path(out, ctx, output)
        }
        Node::Seq { children } => {
            let mut cur = ctx;
            for child in children {
                cur = eval_async(child, cur, dispatcher).await?;
            }
            Ok(cur)
        }
        Node::Branch { cond, then_, else_ } => match eval_expr(cond, &ctx)? {
            Value::Bool(true) => eval_async(then_, ctx, dispatcher).await,
            Value::Bool(false) => eval_async(else_, ctx, dispatcher).await,
            other => Err(EvalError::NonBoolCond(other)),
        },
        Node::Fanout {
            items,
            bind,
            body,
            join,
            out,
        } => fanout_eval(items, bind, body, *join, out, ctx, dispatcher).await,
        Node::Loop {
            counter,
            cond,
            body,
            max,
        } => {
            let mut cur = write_path(counter, ctx, Value::Number(serde_json::Number::from(0u32)))?;
            let mut n: u32 = 0;
            while n < *max && is_truthy(&eval_expr(cond, &cur)?) {
                cur = eval_async(body, cur, dispatcher).await?;
                n += 1;
                cur = write_path(counter, cur, Value::Number(serde_json::Number::from(n)))?;
            }
            Ok(cur)
        }
        Node::Try {
            body,
            catch,
            err_at,
        } => match eval_async(body, ctx.clone(), dispatcher).await {
            Ok(v) => Ok(v),
            Err(e) => {
                let cur = match err_at {
                    Some(at) => write_path(at, ctx, Value::String(e.to_string()))?,
                    None => ctx,
                };
                eval_async(catch, cur, dispatcher).await
            }
        },
    }
}

/// Fanout 並列 evaluator。 executor 不問 (futures crate のみ)、 caller の async
/// runtime (tokio / async-std / 自前) がそのまま並列性を出す。
#[async_recursion]
async fn fanout_eval<D>(
    items: &Expr,
    bind: &Expr,
    body: &Node,
    join: JoinMode,
    out: &Expr,
    ctx: Value,
    dispatcher: &D,
) -> Result<Value, EvalError>
where
    D: AsyncDispatcher + ?Sized,
{
    use futures::future::{join_all, select_ok, FutureExt};

    let items_val = eval_expr(items, &ctx)?;
    let items_arr = match items_val {
        Value::Array(a) => a,
        other => {
            return Err(EvalError::DispatcherError {
                ref_: "fanout.items".into(),
                msg: format!("expected array, got {other:?}"),
            })
        }
    };

    // 各 branch を Pin<Box<dyn Future>> として並列化、 ctx は caller の snapshot を
    // clone して各 branch に渡す (= disjoint state)。
    let branch_futs: Vec<_> = items_arr
        .into_iter()
        .map(|item| {
            let branch_ctx = write_path(bind, ctx.clone(), item)?;
            Ok::<_, EvalError>(eval_async(body, branch_ctx, dispatcher))
        })
        .collect::<Result<_, _>>()?;

    let joined: Value = match join {
        JoinMode::All => {
            // try_join_all = 全成功で Vec、 1 つでも fail で即 abort + error
            let results = futures::future::try_join_all(branch_futs).await?;
            Value::Array(results)
        }
        JoinMode::Any => {
            if branch_futs.is_empty() {
                Value::Array(vec![])
            } else {
                // select_ok = 最初に成功した branch の ctx を winner、 全 fail で last error
                let mapped = branch_futs
                    .into_iter()
                    .map(|f| f.boxed())
                    .collect::<Vec<_>>();
                let (winner, _rest) = select_ok(mapped).await?;
                winner
            }
        }
        JoinMode::Race => {
            if branch_futs.is_empty() {
                Value::Array(vec![])
            } else {
                // select = 最初に settle した branch (Ok / Err 問わず) の結果
                let mapped = branch_futs
                    .into_iter()
                    .map(|f| f.boxed())
                    .collect::<Vec<_>>();
                let (first, _idx, _rest) = futures::future::select_all(mapped).await;
                first?
            }
        }
        JoinMode::AllSettled => {
            // 全 branch 完走、 fail も rejected record として残す
            let results = join_all(branch_futs).await;
            let records: Vec<Value> = results
                .into_iter()
                .map(|r| match r {
                    Ok(v) => serde_json::json!({"status": "fulfilled", "value": v}),
                    Err(e) => serde_json::json!({"status": "rejected", "reason": e.to_string()}),
                })
                .collect();
            Value::Array(records)
        }
    };

    write_path(out, ctx, joined)
}

// ══════════════════════════════════════════════════════════════════════════
// v0.0.3 — mlua bridge full
// ══════════════════════════════════════════════════════════════════════════

use mlua::LuaSerdeExt;

/// Lua function を Rust `Dispatcher` trait に wrap した adapter。
///
/// Lua 側 dispatcher function `function(ref, input) return ... end` を受けて、
/// Rust `eval(node, ctx, &lua_dispatcher)` から呼び出せるようにする。
/// 内部で serde Value ↔ Lua value 変換 (= mlua serde feature) を経由。
struct LuaDispatcher<'a> {
    lua: &'a mlua::Lua,
    func: mlua::Function,
}

impl<'a> Dispatcher for LuaDispatcher<'a> {
    fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError> {
        let lua_input = self
            .lua
            .to_value(&input)
            .map_err(|e| EvalError::DispatcherError {
                ref_: ref_.into(),
                msg: format!("to_value: {}", e),
            })?;
        let result: mlua::Value = self.func.call((ref_.to_string(), lua_input)).map_err(|e| {
            EvalError::DispatcherError {
                ref_: ref_.into(),
                msg: format!("lua call: {}", e),
            }
        })?;
        let value: Value = self
            .lua
            .from_value(result)
            .map_err(|e| EvalError::DispatcherError {
                ref_: ref_.into(),
                msg: format!("from_value: {}", e),
            })?;
        Ok(value)
    }
}

/// Register the flow module table with Lua.
///
/// v0.0.3 full impl — exposes:
///
/// - `flow.version` (= string): crate version
/// - `flow.eval(node_table, ctx_table, dispatcher_fn) -> result_table`:
///   Lua-side entry to evaluate a flow.ir BluePrint with a Lua dispatcher fn
///
/// # Lua usage
///
/// ```lua
/// local flow = require("flow")  -- or set via lua.globals():set("flow", module(lua))
///
/// local node = {
///   kind = "step",
///   ref = "uppercase",
///   ["in"] = { op = "path", at = "$.input" },
///   out = { op = "path", at = "$.output" },
/// }
///
/// local function dispatcher(ref, input)
///   if ref == "uppercase" then
///     return string.upper(input)
///   end
/// end
///
/// local result = flow.eval(node, { input = "hello" }, dispatcher)
/// assert(result.output == "HELLO")
/// ```
pub fn module(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    t.set("version", env!("CARGO_PKG_VERSION"))?;

    let eval_fn = lua.create_function(
        |lua_inner: &mlua::Lua,
         (node_val, ctx_val, dispatcher_fn): (mlua::Value, mlua::Value, mlua::Function)| {
            let node: Node = lua_inner
                .from_value(node_val)
                .map_err(|e| mlua::Error::external(format!("node parse: {}", e)))?;
            let ctx: Value = lua_inner
                .from_value(ctx_val)
                .map_err(|e| mlua::Error::external(format!("ctx parse: {}", e)))?;

            let dispatcher = LuaDispatcher {
                lua: lua_inner,
                func: dispatcher_fn,
            };
            let result = eval(&node, ctx, &dispatcher)
                .map_err(|e| mlua::Error::external(format!("eval: {}", e)))?;
            lua_inner.to_value(&result)
        },
    )?;
    t.set("eval", eval_fn)?;

    Ok(t)
}
