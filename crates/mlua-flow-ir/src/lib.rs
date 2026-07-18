#![deny(unsafe_code)]
#![warn(missing_docs)]
//! flow.ir async runtime + mlua binding.
//!
//! Layer 3 of the 4-layer flow.ir stack:
//!
//! 1. `flow-ir-lua` — Pure Lua DSL (separate repo, ecosystem-neutral)
//! 2. `flow-ir-core` — Pure Rust schema + sync interpreter (no mlua, no async)
//! 3. `mlua-flow-ir` — **this crate**: re-export of `flow-ir-core` +
//!    `AsyncDispatcher` + `eval_async` (including `Fanout` join-mode
//!    support) + Lua `module()` binding
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
//!
//! ## Sync/async divergence (`Fanout` join modes)
//!
//! Sync (`flow_ir_core::eval_with_storage_externs`) and async
//! (`eval_async_with_storage_externs`) share identical per-`Node` logic for
//! `Step` / `Seq` / `Branch` / `Loop` / `Try` / `Let`, and for
//! `JoinMode::All`. Two `Fanout` modes are **intentionally** divergent:
//! `Race` — sync evaluates only `items[0]`; async races every branch and the
//! first branch to *complete* wins, so an early error can win over a later
//! success. `Any` — sync short-circuits sequentially (later branches never
//! dispatch once one succeeds); async launches every branch concurrently and
//! cancels the losers at their next `.await` point, so in-flight side
//! effects on losing branches may be truncated.
//!
//! ## Feature flags
//!
//! Default = `lua54` + `vendored` (matches every existing consumer). The
//! Lua `module()` binding and its `mlua` dependency live behind the
//! implicit `mlua` feature (auto-enabled by any `lua5x`/`luajit`/`luau`
//! feature). To pick another Lua version: `default-features = false,
//! features = ["luajit", "vendored"]`. To link a system Lua instead of a
//! vendored build, drop `vendored`. For an async-only build with no Lua
//! binding at all (`module()` unavailable): `default-features = false`.

// ──────────────────────────────────────────────────────────────────────────
// Re-export Pure Rust core (flow-ir-core)
// ──────────────────────────────────────────────────────────────────────────

pub use flow_ir_core::{
    eval, eval_expr, eval_expr_with_externs, eval_externs, eval_with_storage,
    eval_with_storage_externs, is_truthy, read_path, write_path, CtxStorage, Dispatcher, EvalError,
    Expr, ExternFn, ExternMap, Externs, JoinMode, MemoryCtx, NoExterns, Node, Path, PathParseError,
};

use serde_json::Value;
use std::sync::Arc;

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
    /// Resolve `ref_` against `input` asynchronously, returning the step's
    /// raw output value.
    async fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError>;
}

/// Storage-backed async evaluator — canonical entry.
///
/// `Arc<dyn CtxStorage>` 経由で ctx を共有することで、 dispatch().await suspend
/// 中に外部 task が同じ ctx に `write` できる (= dynamic State injection 経路)。
/// Step 評価の境界で `ctx.snapshot()` を取って Expr eval に渡す。
pub async fn eval_async_with_storage<D>(
    node: &Node,
    ctx: Arc<dyn CtxStorage>,
    dispatcher: &D,
) -> Result<(), EvalError>
where
    D: AsyncDispatcher + ?Sized,
{
    eval_async_with_storage_externs(node, ctx, dispatcher, &NoExterns).await
}

/// `eval_async_with_storage` + externs registry for `call_extern` Expr
/// resolution. `externs` must be `Sync` so the recursive future stays `Send`
/// (host executors spawn it across threads).
#[async_recursion]
pub async fn eval_async_with_storage_externs<D>(
    node: &Node,
    ctx: Arc<dyn CtxStorage>,
    dispatcher: &D,
    externs: &(dyn Externs + Sync),
) -> Result<(), EvalError>
where
    D: AsyncDispatcher + ?Sized,
{
    match node {
        Node::Step { ref_, in_, out } => {
            // snap は dispatch() **呼出し前** の view。 dispatch().await 中に
            // 外部 task が ctx.write しても、 ここで取った snap は影響を受けず
            // input の値は確定。 write_target の `out` path への write は
            // dispatch 完了後に共有 ctx を直接更新。
            let snap = ctx.snapshot();
            let input = eval_expr_with_externs(in_, &snap, externs)?;
            let output =
                dispatcher
                    .dispatch(ref_, input)
                    .await
                    .map_err(|e| EvalError::DispatcherError {
                        ref_: ref_.clone(),
                        msg: e.to_string(),
                    })?;
            ctx.write(&path_of_async(out)?.to_string(), output)
        }
        Node::Seq { children } => {
            for child in children {
                eval_async_with_storage_externs(child, ctx.clone(), dispatcher, externs).await?;
            }
            Ok(())
        }
        Node::Branch { cond, then_, else_ } => {
            let snap = ctx.snapshot();
            match eval_expr_with_externs(cond, &snap, externs)? {
                Value::Bool(true) => {
                    eval_async_with_storage_externs(then_, ctx, dispatcher, externs).await
                }
                Value::Bool(false) => {
                    eval_async_with_storage_externs(else_, ctx, dispatcher, externs).await
                }
                other => Err(EvalError::NonBoolCond(other)),
            }
        }
        Node::Fanout {
            items,
            bind,
            body,
            join,
            out,
        } => fanout_eval(items, bind, body, *join, out, ctx, dispatcher, externs).await,
        Node::Loop {
            counter,
            cond,
            body,
            max,
        } => {
            let counter_path = path_of_async(counter)?.to_string();
            ctx.write(&counter_path, Value::Number(serde_json::Number::from(0u32)))?;
            let mut n: u32 = 0;
            loop {
                if n >= *max {
                    break;
                }
                let snap = ctx.snapshot();
                if !is_truthy(&eval_expr_with_externs(cond, &snap, externs)?) {
                    break;
                }
                eval_async_with_storage_externs(body, ctx.clone(), dispatcher, externs).await?;
                n += 1;
                ctx.write(&counter_path, Value::Number(serde_json::Number::from(n)))?;
            }
            Ok(())
        }
        Node::Try {
            body,
            catch,
            err_at,
        } => {
            let snap_before = ctx.snapshot();
            match eval_async_with_storage_externs(body, ctx.clone(), dispatcher, externs).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    ctx.replace(snap_before);
                    if let Some(at) = err_at {
                        ctx.write(
                            &path_of_async(at)?.to_string(),
                            Value::String(e.to_string()),
                        )?;
                    }
                    eval_async_with_storage_externs(catch, ctx, dispatcher, externs).await
                }
            }
        }
        Node::Let { at, value } => {
            let snap = ctx.snapshot();
            let v = eval_expr_with_externs(value, &snap, externs)?;
            ctx.write(&at.to_string(), v)
        }
    }
}

/// Evaluate a `Node` against a context value asynchronously, using the given
/// async dispatcher for `Step` resolution.
///
/// Legacy Value-passing async evaluator — backward compat wrapper around
/// `eval_async_with_storage` + `MemoryCtx`. 既存 caller (= dynamic injection
/// を要求しない用途) は引き続きこの API で OK。
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
///         in_: Expr::Path { at: "$.input".parse().unwrap() },
///         out: Expr::Path { at: "$.output".parse().unwrap() },
///     };
///     let out = eval_async(&node, json!({ "input": "hello" }), &Fixture).await.unwrap();
///     assert_eq!(out, json!({ "input": "hello", "output": "HELLO" }));
/// });
/// ```
pub async fn eval_async<D>(node: &Node, ctx: Value, dispatcher: &D) -> Result<Value, EvalError>
where
    D: AsyncDispatcher + ?Sized,
{
    eval_async_externs(node, ctx, dispatcher, &NoExterns).await
}

/// `eval_async` + externs registry for `call_extern` Expr resolution.
pub async fn eval_async_externs<D>(
    node: &Node,
    ctx: Value,
    dispatcher: &D,
    externs: &(dyn Externs + Sync),
) -> Result<Value, EvalError>
where
    D: AsyncDispatcher + ?Sized,
{
    let storage: Arc<dyn CtxStorage> = MemoryCtx::shared(ctx);
    eval_async_with_storage_externs(node, storage.clone(), dispatcher, externs).await?;
    Ok(storage.snapshot())
}

/// Extract the already-parsed [`Path`] out of a `Path` `Expr` (async eval
/// side helper — mirrors `flow_ir_core`'s private `path_of`).
fn path_of_async(expr: &Expr) -> Result<&Path, EvalError> {
    match expr {
        Expr::Path { at } => Ok(at),
        _ => Err(EvalError::InvalidPath(
            "expected Path expr for write target".into(),
        )),
    }
}

/// Fanout 並列 evaluator (storage-backed)。 各 branch は disjoint MemoryCtx
/// を持ち、 branch 内で write しても共有 ctx には影響しない (= snapshot 切り出し
/// semantic)。 集約結果は最後に共有 ctx の `out` path に write。
#[async_recursion]
#[allow(clippy::too_many_arguments)]
async fn fanout_eval<D>(
    items: &Expr,
    bind: &Expr,
    body: &Node,
    join: JoinMode,
    out: &Expr,
    ctx: Arc<dyn CtxStorage>,
    dispatcher: &D,
    externs: &(dyn Externs + Sync),
) -> Result<(), EvalError>
where
    D: AsyncDispatcher + ?Sized,
{
    use futures::future::{join_all, select_ok, FutureExt};

    let snap = ctx.snapshot();
    let items_val = eval_expr_with_externs(items, &snap, externs)?;
    let items_arr = match items_val {
        Value::Array(a) => a,
        other => {
            return Err(EvalError::TypeError {
                op: "fanout.items".into(),
                msg: format!("expected array, got {other:?}"),
            })
        }
    };

    // branch storage を pre-allocate して、 各 branch future と pair で持つ。
    // 集約時に同じ storage の snapshot を取って結果にする。
    let branches: Vec<Arc<dyn CtxStorage>> = items_arr
        .into_iter()
        .map(|item| -> Result<Arc<dyn CtxStorage>, EvalError> {
            let branch_ctx = write_path(bind, snap.clone(), item)?;
            Ok(MemoryCtx::shared(branch_ctx))
        })
        .collect::<Result<_, _>>()?;

    // 各 branch を `(idx, future)` で wrap。 future は branch storage と body を
    // 共有して走る。
    let branch_futs: Vec<_> = branches
        .iter()
        .map(|b| eval_async_with_storage_externs(body, b.clone(), dispatcher, externs))
        .collect();

    let joined: Value = match join {
        JoinMode::All => {
            futures::future::try_join_all(branch_futs).await?;
            Value::Array(branches.iter().map(|b| b.snapshot()).collect())
        }
        JoinMode::Any => {
            // Promise.any parity (mirrors the sync side): zero items can
            // never produce a winner, so this raises rather than returning
            // `[]` (unlike All/AllSettled, whose empty-array result shape
            // is still meaningful).
            if branch_futs.is_empty() {
                return Err(EvalError::TypeError {
                    op: "fanout.any".into(),
                    msg: "requires at least one item".into(),
                });
            }
            let mapped: Vec<_> = branch_futs
                .into_iter()
                .enumerate()
                .map(|(i, f)| f.map(move |r| r.map(|()| i)).boxed())
                .collect();
            let (winner_idx, _rest) = select_ok(mapped).await?;
            branches[winner_idx].snapshot()
        }
        JoinMode::Race => {
            // Same rationale as Any: zero branches means there is nothing
            // to race.
            if branch_futs.is_empty() {
                return Err(EvalError::TypeError {
                    op: "fanout.race".into(),
                    msg: "requires at least one item".into(),
                });
            }
            let mapped: Vec<_> = branch_futs
                .into_iter()
                .enumerate()
                .map(|(i, f)| f.map(move |r| r.map(|()| i)).boxed())
                .collect();
            let (first, _idx, _rest) = futures::future::select_all(mapped).await;
            let winner_idx = first?;
            branches[winner_idx].snapshot()
        }
        JoinMode::AllSettled => {
            let results = join_all(branch_futs).await;
            let records: Vec<Value> = results
                .into_iter()
                .zip(branches.iter())
                .map(|(r, b)| match r {
                    Ok(()) => serde_json::json!({"status": "fulfilled", "value": b.snapshot()}),
                    Err(e) => serde_json::json!({"status": "rejected", "reason": e.to_string()}),
                })
                .collect();
            Value::Array(records)
        }
    };

    ctx.write(&path_of_async(out)?.to_string(), joined)
}

// ══════════════════════════════════════════════════════════════════════════
// v0.0.3 — mlua bridge full (feature = "mlua")
// ══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "mlua")]
use mlua::LuaSerdeExt;

/// Lua function を Rust `Dispatcher` trait に wrap した adapter。
///
/// Lua 側 dispatcher function `function(ref, input) return ... end` を受けて、
/// Rust `eval(node, ctx, &lua_dispatcher)` から呼び出せるようにする。
/// 内部で serde Value ↔ Lua value 変換 (= mlua serde feature) を経由。
#[cfg(feature = "mlua")]
struct LuaDispatcher<'a> {
    lua: &'a mlua::Lua,
    func: mlua::Function,
}

#[cfg(feature = "mlua")]
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

/// Lua function table を Rust `Externs` trait に wrap した adapter。
///
/// canonical `opts.externs` (flow-ir-lua) と同型: table の各 entry は
/// pure Lua function で、 `call_extern` Expr の ref で引かれ、 評価済み args
/// を positional に受けて値を返す。 これが「LuaScript 直実行 Hatch」の
/// Rust 側の受け口 (extern の実体は任意の Lua closure)。
#[cfg(feature = "mlua")]
struct LuaExterns<'a> {
    lua: &'a mlua::Lua,
    table: mlua::Table,
}

#[cfg(feature = "mlua")]
impl<'a> flow_ir_core::Externs for LuaExterns<'a> {
    fn call(&self, ref_: &str, args: &[Value]) -> Result<Value, EvalError> {
        let func: mlua::Function = self.table.get(ref_).map_err(|_| EvalError::ExternError {
            ref_: ref_.into(),
            msg: "not registered in externs (or not a function)".into(),
        })?;
        let mut lua_args = mlua::MultiValue::new();
        for a in args {
            lua_args.push_back(self.lua.to_value(a).map_err(|e| EvalError::ExternError {
                ref_: ref_.into(),
                msg: format!("to_value: {}", e),
            })?);
        }
        let result: mlua::Value = func.call(lua_args).map_err(|e| EvalError::ExternError {
            ref_: ref_.into(),
            msg: format!("lua call: {}", e),
        })?;
        self.lua
            .from_value(result)
            .map_err(|e| EvalError::ExternError {
                ref_: ref_.into(),
                msg: format!("from_value: {}", e),
            })
    }
}

/// Register the flow module table with Lua.
///
/// Exposes:
///
/// - `flow.version` (= string): crate version
/// - `flow.eval(node_table, ctx_table, dispatcher_fn, externs_table?) ->
///   result_table`: Lua-side entry to evaluate a flow.ir BluePrint with a
///   Lua dispatcher fn. Optional 4th arg is a table of pure Lua functions
///   resolved by `call_extern` Expr (canonical `opts.externs` parity).
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
///
/// -- call_extern: whitelist pure Lua fns via the externs table
/// local node2 = {
///   kind = "let",
///   at = "ctx.root",
///   value = { op = "call_extern", ref = "math.sqrt",
///             args = { { op = "path", at = "$.n" } } },
/// }
/// local result2 = flow.eval(node2, { n = 9 }, dispatcher,
///                           { ["math.sqrt"] = math.sqrt })
/// assert(result2.root == 3)
/// ```
#[cfg(feature = "mlua")]
pub fn module(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    t.set("version", env!("CARGO_PKG_VERSION"))?;

    let eval_fn = lua.create_function(
        |lua_inner: &mlua::Lua,
         (node_val, ctx_val, dispatcher_fn, externs_val): (
            mlua::Value,
            mlua::Value,
            mlua::Function,
            Option<mlua::Table>,
        )| {
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
            let result = match externs_val {
                Some(table) => {
                    let externs = LuaExterns {
                        lua: lua_inner,
                        table,
                    };
                    eval_externs(&node, ctx, &dispatcher, &externs)
                }
                None => eval(&node, ctx, &dispatcher),
            }
            .map_err(|e| mlua::Error::external(format!("eval: {}", e)))?;
            lua_inner.to_value(&result)
        },
    )?;
    t.set("eval", eval_fn)?;

    Ok(t)
}
