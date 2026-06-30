#![deny(unsafe_code)]
//! flow.ir Pure Rust schema + sync interpreter.
//!
//! 3 Node kinds (Step / Seq / Branch) + Fanout / Loop / Try + 3 Expr ops
//! (Path / Lit / Eq) + sync `eval` + `Dispatcher` trait + Path read/write.
//!
//! mlua / futures / async 依存ゼロ。 async runtime + mlua binding は上流
//! `mlua-flow-ir` crate が担当する 4 層 stack の core 層。
//!
//! # Quick start
//!
//! ```
//! use flow_ir_core::{eval, Dispatcher, EvalError, Expr, Node};
//! use serde_json::{json, Value};
//!
//! let node: Node = serde_json::from_value(json!({
//!     "kind": "step",
//!     "ref": "uppercase",
//!     "in": { "op": "path", "at": "$.input" },
//!     "out": { "op": "path", "at": "$.output" },
//! })).unwrap();
//!
//! struct Fixture;
//! impl Dispatcher for Fixture {
//!     fn dispatch(&self, _r: &str, input: Value) -> Result<Value, EvalError> {
//!         if let Value::String(s) = input {
//!             Ok(Value::String(s.to_uppercase()))
//!         } else {
//!             Ok(input)
//!         }
//!     }
//! }
//!
//! let out = eval(&node, json!({ "input": "hello" }), &Fixture).unwrap();
//! assert_eq!(out, json!({ "input": "hello", "output": "HELLO" }));
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

// ──────────────────────────────────────────────────────────────────────────
// IR: 3 Node kinds + 3 Expr ops
// ──────────────────────────────────────────────────────────────────────────

/// flow.ir Node kind.
///
/// Discriminated with `kind` tag, `deny_unknown_fields` (open=false),
/// `rename_all = "snake_case"`. Parser-side coverage: Step / Seq / Branch +
/// Fanout (canonical schema の `fanout` Node、 4 join mode)。 残り Node kind
/// (let / loop / call / switch / try / map / reduce / etc) は別 turn carry。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "snake_case")]
pub enum Node {
    /// `Step` — dispatch a referenced operation with `in` input, write result to `out`.
    Step {
        #[serde(rename = "ref")]
        ref_: String,
        #[serde(rename = "in")]
        in_: Expr,
        out: Expr,
    },
    /// `Seq` — evaluate children in order, threading the context value through.
    Seq { children: Vec<Node> },
    /// `Branch` — eval `cond`; if `true` run `then`, else run `else`.
    Branch {
        cond: Expr,
        #[serde(rename = "then")]
        then_: Box<Node>,
        #[serde(rename = "else")]
        else_: Box<Node>,
    },
    /// `Fanout` — eval `items` to an array, run `body` per item against a
    /// branch-local ctx (caller ctx + item written to `bind`), join results
    /// per `join` mode into `out`. Async parallel runner uses
    /// `futures::future::{try_join_all|select_ok|join_all}` (executor-agnostic).
    Fanout {
        items: Expr,
        bind: Expr,
        body: Box<Node>,
        join: JoinMode,
        out: Expr,
    },
    /// `Loop` — counter を 0 から、 `cond` が truthy かつ `counter < max` の間
    /// `body` を eval。 各 iter 後 counter を increment して `counter` path に書く。
    /// VerdictLoop 等の retry/poll パターン primitive (canonical schema 整合)。
    Loop {
        counter: Expr,
        cond: Expr,
        body: Box<Node>,
        max: u32,
    },
    /// `Try` — `body` を eval、 raise した場合 `catch` を eval。
    /// `err_at` が Some なら catch 開始前に error message を ctx に書く。
    Try {
        body: Box<Node>,
        catch: Box<Node>,
        #[serde(default)]
        err_at: Option<Expr>,
    },
    /// `Assign` — pure transform Node。 `value` Expr を ctx snapshot 上で評価し、
    /// 結果を `at` (Path Expr) に write する。 dispatcher 不要、 副作用は
    /// `CtxStorage.write` 1 回のみ。 `Seq` の中で Step 間の Adhoc update 表現に
    /// 使う (= IR primitive、 Command 履歴は CtxStorage の write hook 経由で取得)。
    Assign {
        at: Expr,
        value: Expr,
    },
}

/// Fanout join semantics (Promise / futures combinators).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinMode {
    /// every branch runs; out is an array of per-branch final ctx
    /// (Promise.all / `futures::try_join_all`).
    All,
    /// first non-raising branch's ctx wins; all-fail raises
    /// (Promise.any / `futures::future::select_ok`).
    Any,
    /// first branch to settle wins, success OR raise
    /// (Promise.race / `futures::future::select`).
    Race,
    /// every branch runs, never raises; per-item record
    /// `{status: fulfilled|rejected, value|reason}` (Promise.allSettled).
    AllSettled,
}

/// flow.ir Expr op.
///
/// Discriminated with `op` tag, `deny_unknown_fields`, `rename_all = "snake_case"`.
/// MVP scope: Path / Lit / Eq only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", deny_unknown_fields, rename_all = "snake_case")]
pub enum Expr {
    /// `Path` — read a value from ctx by simple `$.a.b.c` form.
    Path { at: String },
    /// `Lit` — literal JSON value.
    Lit { value: Value },
    /// `Eq` — boolean equality of two sub-expressions.
    Eq { lhs: Box<Expr>, rhs: Box<Expr> },
}

// ──────────────────────────────────────────────────────────────────────────
// Dispatcher trait + EvalError
// ──────────────────────────────────────────────────────────────────────────

/// Dispatcher callback: resolves a `Step.ref` against the provided input,
/// returns the step's raw output value.
///
/// Host crates (e.g. `mlua-swarm-engine`) provide concrete implementations:
/// agent-block process spawn, mlua callback, MCP call, direct LLM, etc.
/// `Fn(&str, Value) -> Result<Value, EvalError>` closures also implement this
/// trait via the blanket impl below.
pub trait Dispatcher {
    fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError>;
}

impl<F> Dispatcher for F
where
    F: Fn(&str, Value) -> Result<Value, EvalError>,
{
    fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError> {
        self(ref_, input)
    }
}

/// Evaluation error.
#[derive(Debug, Error)]
pub enum EvalError {
    #[error("path not found: {0}")]
    PathNotFound(String),
    #[error("invalid path syntax: {0}")]
    InvalidPath(String),
    #[error("branch cond must be boolean, got: {0}")]
    NonBoolCond(Value),
    #[error("dispatcher error for ref '{ref_}': {msg}")]
    DispatcherError { ref_: String, msg: String },
}

// ──────────────────────────────────────────────────────────────────────────
// CtxStorage — Backend DI for ctx state
// ──────────────────────────────────────────────────────────────────────────

/// Ctx backend trait — `eval(_with_storage)` 系が ctx state を touch する
/// 唯一の経路。 `&self` write (interior mutability) で **走行中の Flow と
/// 外部 task が同じ ctx を共有** できる (= dispatch().await suspend 中に外部
/// task が `ctx.write` で State 注入 → resume 後 Step が read で観測、 という
/// dynamic injection 経路を成立させる)。
///
/// Default impl は `MemoryCtx` (`Arc<Mutex<Value>>` wrapper、 既存
/// `serde_json::Value` 直保持と挙動互換)。 consumer は typed struct / KV /
/// 外部 store / observer wrap / event log 等を custom impl で持ち込める。
pub trait CtxStorage: Send + Sync {
    /// Read a single path (`$.a.b.c` 形式) from ctx.
    fn read(&self, path: &str) -> Result<Value, EvalError>;
    /// Write `value` to `path` (`$.a.b.c` 形式).
    fn write(&self, path: &str, value: Value) -> Result<(), EvalError>;
    /// Take a snapshot of the entire ctx (= Expr eval / Fanout fork で使う pure read view).
    fn snapshot(&self) -> Value;
    /// Replace the entire ctx with the given value (= Fanout branch restore 等).
    fn replace(&self, value: Value);
}

/// Default `CtxStorage` impl — `Arc<Mutex<Value>>` wrapper。
///
/// Send + Sync かつ `&self` write OK = `Arc<MemoryCtx>` で外部 task と共有可能。
pub struct MemoryCtx {
    inner: std::sync::Mutex<Value>,
}

impl MemoryCtx {
    /// Create a new MemoryCtx initialised with `ctx`.
    pub fn new(ctx: Value) -> Self {
        Self {
            inner: std::sync::Mutex::new(ctx),
        }
    }

    /// Convenience: wrap in `Arc<dyn CtxStorage>`.
    pub fn shared(ctx: Value) -> std::sync::Arc<dyn CtxStorage> {
        std::sync::Arc::new(Self::new(ctx))
    }
}

impl CtxStorage for MemoryCtx {
    fn read(&self, path: &str) -> Result<Value, EvalError> {
        let guard = self.inner.lock().expect("ctx mutex poisoned");
        read_path(path, &guard)
    }

    fn write(&self, path: &str, value: Value) -> Result<(), EvalError> {
        let mut guard = self.inner.lock().expect("ctx mutex poisoned");
        let cur = std::mem::take(&mut *guard);
        let updated = write_path(&Expr::Path { at: path.to_string() }, cur, value)?;
        *guard = updated;
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let guard = self.inner.lock().expect("ctx mutex poisoned");
        guard.clone()
    }

    fn replace(&self, value: Value) {
        let mut guard = self.inner.lock().expect("ctx mutex poisoned");
        *guard = value;
    }
}

/// Resolve `Path` Expr to its literal `$.a.b.c` string, or `InvalidPath` error.
fn path_str(expr: &Expr) -> Result<&str, EvalError> {
    match expr {
        Expr::Path { at } => Ok(at.as_str()),
        _ => Err(EvalError::InvalidPath(
            "expected Path expr for write target".into(),
        )),
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Evaluator — storage-backed (canonical) + legacy Value-passing wrapper
// ──────────────────────────────────────────────────────────────────────────

/// Storage-backed sync evaluator — `CtxStorage` 経由で ctx を touch する正本。
///
/// 各 Node 評価開始時に `ctx.snapshot()` で Expr eval 用の pure view を取り、
/// write は `ctx.write(path, value)` 経由。 これにより同じ `Arc<dyn CtxStorage>`
/// を共有する外部 task が、 Step 間 (sync の場合は 1 Step 評価内では touch
/// しないが) や eval 間で ctx state を変更できる。
pub fn eval_with_storage<D: Dispatcher>(
    node: &Node,
    ctx: &dyn CtxStorage,
    dispatcher: &D,
) -> Result<(), EvalError> {
    match node {
        Node::Step { ref_, in_, out } => {
            let snap = ctx.snapshot();
            let input = eval_expr(in_, &snap)?;
            let output =
                dispatcher
                    .dispatch(ref_, input)
                    .map_err(|e| EvalError::DispatcherError {
                        ref_: ref_.clone(),
                        msg: e.to_string(),
                    })?;
            ctx.write(path_str(out)?, output)
        }
        Node::Seq { children } => {
            for child in children {
                eval_with_storage(child, ctx, dispatcher)?;
            }
            Ok(())
        }
        Node::Branch { cond, then_, else_ } => {
            let snap = ctx.snapshot();
            match eval_expr(cond, &snap)? {
                Value::Bool(true) => eval_with_storage(then_, ctx, dispatcher),
                Value::Bool(false) => eval_with_storage(else_, ctx, dispatcher),
                other => Err(EvalError::NonBoolCond(other)),
            }
        }
        Node::Fanout {
            items,
            bind,
            body,
            join,
            out,
        } => {
            // Fanout fork = 各 branch を disjoint MemoryCtx に切り出して逐次
            // (sync) evaluate、 集約結果を共有 ctx の `out` path に書く。
            let snap = ctx.snapshot();
            let items_val = eval_expr(items, &snap)?;
            let items_arr = match items_val {
                Value::Array(a) => a,
                other => {
                    return Err(EvalError::DispatcherError {
                        ref_: "fanout.items".into(),
                        msg: format!("expected array, got {other:?}"),
                    })
                }
            };
            let joined = fanout_eval_sync(bind, body, *join, &snap, items_arr, dispatcher)?;
            ctx.write(path_str(out)?, joined)
        }
        Node::Loop {
            counter,
            cond,
            body,
            max,
        } => {
            let counter_path = path_str(counter)?;
            ctx.write(counter_path, Value::Number(serde_json::Number::from(0u32)))?;
            let mut n: u32 = 0;
            loop {
                if n >= *max {
                    break;
                }
                let snap = ctx.snapshot();
                if !is_truthy(&eval_expr(cond, &snap)?) {
                    break;
                }
                eval_with_storage(body, ctx, dispatcher)?;
                n += 1;
                ctx.write(counter_path, Value::Number(serde_json::Number::from(n)))?;
            }
            Ok(())
        }
        Node::Try {
            body,
            catch,
            err_at,
        } => {
            // body 失敗時の rollback 用 snapshot
            let snap_before = ctx.snapshot();
            match eval_with_storage(body, ctx, dispatcher) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // body の途中 write を破棄 (Try semantic: rollback)
                    ctx.replace(snap_before);
                    if let Some(at) = err_at {
                        ctx.write(path_str(at)?, Value::String(e.to_string()))?;
                    }
                    eval_with_storage(catch, ctx, dispatcher)
                }
            }
        }
        Node::Assign { at, value } => {
            let snap = ctx.snapshot();
            let v = eval_expr(value, &snap)?;
            ctx.write(path_str(at)?, v)
        }
    }
}

/// Internal: fanout per-item sync evaluator (disjoint branch ctx).
fn fanout_eval_sync<D: Dispatcher>(
    bind: &Expr,
    body: &Node,
    join: JoinMode,
    base_snap: &Value,
    items_arr: Vec<Value>,
    dispatcher: &D,
) -> Result<Value, EvalError> {
    match join {
        JoinMode::All => {
            let mut results = Vec::with_capacity(items_arr.len());
            for item in items_arr {
                let branch_ctx = write_path(bind, base_snap.clone(), item)?;
                let storage = MemoryCtx::new(branch_ctx);
                eval_with_storage(body, &storage, dispatcher)?;
                results.push(storage.snapshot());
            }
            Ok(Value::Array(results))
        }
        JoinMode::Any => {
            let mut winner: Option<Value> = None;
            let mut last_err: Option<EvalError> = None;
            for item in items_arr {
                let branch_ctx = write_path(bind, base_snap.clone(), item)?;
                let storage = MemoryCtx::new(branch_ctx);
                match eval_with_storage(body, &storage, dispatcher) {
                    Ok(()) => {
                        winner = Some(storage.snapshot());
                        last_err = None;
                        break;
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            if let Some(e) = last_err {
                return Err(e);
            }
            Ok(winner.unwrap_or(Value::Array(vec![])))
        }
        JoinMode::Race => {
            if let Some(first) = items_arr.into_iter().next() {
                let branch_ctx = write_path(bind, base_snap.clone(), first)?;
                let storage = MemoryCtx::new(branch_ctx);
                eval_with_storage(body, &storage, dispatcher)?;
                Ok(storage.snapshot())
            } else {
                Ok(Value::Array(vec![]))
            }
        }
        JoinMode::AllSettled => {
            let mut records = Vec::with_capacity(items_arr.len());
            for item in items_arr {
                let branch_ctx = write_path(bind, base_snap.clone(), item)?;
                let storage = MemoryCtx::new(branch_ctx);
                match eval_with_storage(body, &storage, dispatcher) {
                    Ok(()) => records
                        .push(serde_json::json!({"status": "fulfilled", "value": storage.snapshot()})),
                    Err(e) => records.push(
                        serde_json::json!({"status": "rejected", "reason": e.to_string()}),
                    ),
                }
            }
            Ok(Value::Array(records))
        }
    }
}

/// Legacy Value-passing sync evaluator — backward compat wrapper around
/// `eval_with_storage` + `MemoryCtx`. `Value` を所有権で受け取り、 内部で
/// `MemoryCtx::new(ctx)` を使って storage 版に委譲、 終了後の snapshot を返す。
///
/// 既存 caller (= dynamic injection を要求しない、 1-shot pure eval 用途) は
/// 引き続きこの API で OK。 動的注入が要る場合は `eval_with_storage` を直接
/// 呼ぶ。
///
/// Returns the updated context (= ctx with `Step.out` path written for each step traversed).
pub fn eval<D: Dispatcher>(node: &Node, ctx: Value, dispatcher: &D) -> Result<Value, EvalError> {
    let storage = MemoryCtx::new(ctx);
    eval_with_storage(node, &storage, dispatcher)?;
    Ok(storage.snapshot())
}

/// JSON value の truthy 判定 (= flow.ir Branch cond / Loop cond で使う)。
/// Bool は値そのまま、 null/false 以外は truthy (Lua / JS と整合)。
pub fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        _ => true,
    }
}

/// Evaluate an `Expr` against a context value, returning the resolved JSON value.
pub fn eval_expr(expr: &Expr, ctx: &Value) -> Result<Value, EvalError> {
    match expr {
        Expr::Lit { value } => Ok(value.clone()),
        Expr::Path { at } => read_path(at, ctx),
        Expr::Eq { lhs, rhs } => {
            let lv = eval_expr(lhs, ctx)?;
            let rv = eval_expr(rhs, ctx)?;
            Ok(Value::Bool(lv == rv))
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Path helpers (simple `$.a.b.c` form, no array index in MVP)
// ──────────────────────────────────────────────────────────────────────────

/// Read a path from a JSON value. Supports simple `$.a.b.c` form.
pub fn read_path(path: &str, ctx: &Value) -> Result<Value, EvalError> {
    let trimmed = strip_path_prefix(path)?;
    if trimmed.is_empty() {
        return Ok(ctx.clone());
    }
    let mut cur = ctx;
    for key in trimmed.split('.') {
        cur = cur
            .get(key)
            .ok_or_else(|| EvalError::PathNotFound(path.to_string()))?;
    }
    Ok(cur.clone())
}

/// Write a value at the path location inside ctx, returning the updated ctx.
/// `out` must be a `Path` Expr.
pub fn write_path(out: &Expr, ctx: Value, value: Value) -> Result<Value, EvalError> {
    let path = match out {
        Expr::Path { at } => at,
        _ => {
            return Err(EvalError::InvalidPath(
                "Step.out must be a Path expr".into(),
            ))
        }
    };
    let trimmed = strip_path_prefix(path)?;
    let keys: Vec<&str> = trimmed.split('.').filter(|s| !s.is_empty()).collect();
    if keys.is_empty() {
        return Ok(value);
    }
    let mut root = ctx;
    write_path_recursive(&mut root, &keys, value);
    Ok(root)
}

fn strip_path_prefix(path: &str) -> Result<&str, EvalError> {
    path.strip_prefix("$.")
        .or_else(|| path.strip_prefix('$'))
        .ok_or_else(|| EvalError::InvalidPath(format!("path must start with $ or $.: {}", path)))
}

fn write_path_recursive(node: &mut Value, keys: &[&str], value: Value) {
    if keys.is_empty() {
        *node = value;
        return;
    }
    if !node.is_object() {
        *node = Value::Object(serde_json::Map::new());
    }
    let obj = node.as_object_mut().expect("just initialised as object");
    let key = keys[0];
    if keys.len() == 1 {
        obj.insert(key.to_string(), value);
    } else {
        let entry = obj
            .entry(key.to_string())
            .or_insert(Value::Object(serde_json::Map::new()));
        write_path_recursive(entry, &keys[1..], value);
    }
}
