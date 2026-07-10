#![deny(unsafe_code)]
//! flow.ir Pure Rust schema + sync interpreter.
//!
//! Node kinds (Step / Seq / Branch / Fanout / Loop / Try / Assign) + Expr ops
//! (canonical wire format — comparison / boolean / existence / arithmetic /
//! aggregate / `call_extern`) + sync `eval` + `Dispatcher` trait + `Externs`
//! registry + Path read/write.
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
    Assign { at: Expr, value: Expr },
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
/// Wire format (op tag / field names) follows the canonical `flow-ir-lua`
/// schema (`flow/ir/schema.lua`) verbatim: `gte`/`lte` (not `ge`/`le`),
/// `args` on `and`/`or`, `arg` on `not`/`len`/`exists`.
///
/// Ops:
/// - read / literal: `Path` / `Lit`
/// - comparison: `Eq` / `Ne` / `Lt` / `Lte` / `Gt` / `Gte` (numbers or strings)
/// - boolean: `Not` / `And` / `Or`
/// - existence: `Exists` (truthy iff `arg` evaluates to a non-null value)
/// - arithmetic: `Add` / `Sub` / `Mul` / `Div` / `Mod`
/// - aggregate: `Len` (length of array / string / object) / `In` (membership in array)
/// - hatch: `CallExtern` (host-registered pure function, resolved via `Externs`)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", deny_unknown_fields, rename_all = "snake_case")]
pub enum Expr {
    /// `Path` — read a value from ctx by simple `$.a.b.c` form.
    Path { at: String },
    /// `Lit` — literal JSON value.
    Lit { value: Value },
    /// `Eq` — boolean equality of two sub-expressions.
    Eq { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Ne` — boolean inequality.
    Ne { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Lt` — `lhs < rhs`. Both numbers (f64) or both strings (lexicographic),
    /// mirroring canonical Lua `<` semantics. Mixed / other types raise.
    Lt { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Lte` — `lhs <= rhs` (canonical wire tag `lte`).
    Lte { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Gt` — `lhs > rhs`.
    Gt { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Gte` — `lhs >= rhs` (canonical wire tag `gte`).
    Gte { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Not` — boolean negation of `arg` (truthy-based; null/false → true).
    Not { arg: Box<Expr> },
    /// `And` — variadic boolean conjunction (short-circuit). Empty list → true.
    And { args: Vec<Expr> },
    /// `Or` — variadic boolean disjunction (short-circuit). Empty list → false.
    Or { args: Vec<Expr> },
    /// `Exists` — evaluate `arg`; `true` iff it resolves to a non-null value.
    /// A `Path` arg that raises `PathNotFound` yields `false` (canonical
    /// `arg ~= nil` semantics — JSON null maps to Lua nil).
    Exists { arg: Box<Expr> },
    /// `Add` — numeric `lhs + rhs` (f64).
    Add { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Sub` — numeric `lhs - rhs`.
    Sub { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Mul` — numeric `lhs * rhs`.
    Mul { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Div` — numeric `lhs / rhs`. Division by zero raises `DispatcherError`.
    Div { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Mod` — numeric `lhs % rhs` (Lua `%` semantics: result takes the sign
    /// of `rhs`). Modulo by zero raises `DispatcherError`.
    Mod { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `Len` — length of `arg`: array → element count, string → char count,
    /// object → key count. Other types raise `DispatcherError`.
    Len { arg: Box<Expr> },
    /// `In` — `true` if `needle` equals any element of `haystack` (which must
    /// evaluate to an array). Rust-side extension (not in canonical schema).
    In {
        needle: Box<Expr>,
        haystack: Box<Expr>,
    },
    /// `CallExtern` — value-shape Hatch: resolve a host-injected pure function
    /// by opaque key via the `Externs` registry, apply it to evaluated args,
    /// return the value. The registered function MUST be pure (no side
    /// effects, no flow control) — see canonical `doc/ir.md §call_extern`.
    CallExtern {
        #[serde(rename = "ref")]
        ref_: String,
        args: Vec<Expr>,
    },
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
    #[error("extern error for ref '{ref_}': {msg}")]
    ExternError { ref_: String, msg: String },
}

// ──────────────────────────────────────────────────────────────────────────
// Externs — whitelist registry for `call_extern` Expr (canonical opts.externs)
// ──────────────────────────────────────────────────────────────────────────

/// Extern registry: resolves a `call_extern.ref` against evaluated args and
/// returns the value. Mirror of canonical `opts.externs` (flow-ir-lua
/// `interpreter.lua`): each entry MUST be a pure function — no side effects,
/// no flow control, value-shape manipulation only.
///
/// Same DI pattern as [`Dispatcher`]: host crates provide concrete
/// implementations ([`ExternMap`] for plain Rust closures, mlua bridge for
/// Lua functions upstream).
pub trait Externs {
    /// Invoke the extern registered under `ref_` with already-evaluated args.
    /// Unregistered refs raise [`EvalError::ExternError`].
    fn call(&self, ref_: &str, args: &[Value]) -> Result<Value, EvalError>;
}

/// Empty registry — every `call_extern` raises `ExternError` (parity with
/// canonical "requires opts.externs" error). Used by the externs-less
/// compat wrappers (`eval` / `eval_expr` / `eval_with_storage`).
pub struct NoExterns;

impl Externs for NoExterns {
    fn call(&self, ref_: &str, _args: &[Value]) -> Result<Value, EvalError> {
        Err(EvalError::ExternError {
            ref_: ref_.into(),
            msg: "no externs registry configured".into(),
        })
    }
}

/// Boxed pure extern function stored in [`ExternMap`].
pub type ExternFn = Box<dyn Fn(&[Value]) -> Result<Value, EvalError> + Send + Sync>;

/// `HashMap`-backed [`Externs`] impl for host-side Rust closures.
///
/// ```
/// use flow_ir_core::{eval_expr_with_externs, EvalError, Expr, ExternMap};
/// use serde_json::{json, Value};
///
/// let mut externs = ExternMap::new();
/// externs.register("math.sqrt", |args: &[Value]| {
///     let x = args[0].as_f64().ok_or_else(|| EvalError::ExternError {
///         ref_: "math.sqrt".into(),
///         msg: "expected number".into(),
///     })?;
///     Ok(json!(x.sqrt()))
/// });
///
/// let expr: Expr = serde_json::from_value(json!({
///     "op": "call_extern", "ref": "math.sqrt",
///     "args": [{ "op": "lit", "value": 9.0 }],
/// })).unwrap();
/// let out = eval_expr_with_externs(&expr, &json!({}), &externs).unwrap();
/// assert_eq!(out, json!(3.0));
/// ```
#[derive(Default)]
pub struct ExternMap {
    fns: std::collections::HashMap<String, ExternFn>,
}

impl ExternMap {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pure function under `name` (overwrites an existing entry).
    pub fn register<F>(&mut self, name: impl Into<String>, f: F)
    where
        F: Fn(&[Value]) -> Result<Value, EvalError> + Send + Sync + 'static,
    {
        self.fns.insert(name.into(), Box::new(f));
    }

    /// Whether `name` is registered (compile-time whitelist check parity).
    pub fn contains(&self, name: &str) -> bool {
        self.fns.contains_key(name)
    }
}

impl Externs for ExternMap {
    fn call(&self, ref_: &str, args: &[Value]) -> Result<Value, EvalError> {
        let f = self.fns.get(ref_).ok_or_else(|| EvalError::ExternError {
            ref_: ref_.into(),
            msg: "not registered in externs".into(),
        })?;
        f(args)
    }
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
        let updated = write_path(
            &Expr::Path {
                at: path.to_string(),
            },
            cur,
            value,
        )?;
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
    eval_with_storage_externs(node, ctx, dispatcher, &NoExterns)
}

/// `eval_with_storage` + externs registry for `call_extern` Expr resolution.
pub fn eval_with_storage_externs<D: Dispatcher>(
    node: &Node,
    ctx: &dyn CtxStorage,
    dispatcher: &D,
    externs: &dyn Externs,
) -> Result<(), EvalError> {
    match node {
        Node::Step { ref_, in_, out } => {
            let snap = ctx.snapshot();
            let input = eval_expr_with_externs(in_, &snap, externs)?;
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
                eval_with_storage_externs(child, ctx, dispatcher, externs)?;
            }
            Ok(())
        }
        Node::Branch { cond, then_, else_ } => {
            let snap = ctx.snapshot();
            match eval_expr_with_externs(cond, &snap, externs)? {
                Value::Bool(true) => eval_with_storage_externs(then_, ctx, dispatcher, externs),
                Value::Bool(false) => eval_with_storage_externs(else_, ctx, dispatcher, externs),
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
            let items_val = eval_expr_with_externs(items, &snap, externs)?;
            let items_arr = match items_val {
                Value::Array(a) => a,
                other => {
                    return Err(EvalError::DispatcherError {
                        ref_: "fanout.items".into(),
                        msg: format!("expected array, got {other:?}"),
                    })
                }
            };
            let joined =
                fanout_eval_sync(bind, body, *join, &snap, items_arr, dispatcher, externs)?;
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
                if !is_truthy(&eval_expr_with_externs(cond, &snap, externs)?) {
                    break;
                }
                eval_with_storage_externs(body, ctx, dispatcher, externs)?;
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
            match eval_with_storage_externs(body, ctx, dispatcher, externs) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // body の途中 write を破棄 (Try semantic: rollback)
                    ctx.replace(snap_before);
                    if let Some(at) = err_at {
                        ctx.write(path_str(at)?, Value::String(e.to_string()))?;
                    }
                    eval_with_storage_externs(catch, ctx, dispatcher, externs)
                }
            }
        }
        Node::Assign { at, value } => {
            let snap = ctx.snapshot();
            let v = eval_expr_with_externs(value, &snap, externs)?;
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
    externs: &dyn Externs,
) -> Result<Value, EvalError> {
    match join {
        JoinMode::All => {
            let mut results = Vec::with_capacity(items_arr.len());
            for item in items_arr {
                let branch_ctx = write_path(bind, base_snap.clone(), item)?;
                let storage = MemoryCtx::new(branch_ctx);
                eval_with_storage_externs(body, &storage, dispatcher, externs)?;
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
                match eval_with_storage_externs(body, &storage, dispatcher, externs) {
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
                eval_with_storage_externs(body, &storage, dispatcher, externs)?;
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
                match eval_with_storage_externs(body, &storage, dispatcher, externs) {
                    Ok(()) => records.push(
                        serde_json::json!({"status": "fulfilled", "value": storage.snapshot()}),
                    ),
                    Err(e) => records
                        .push(serde_json::json!({"status": "rejected", "reason": e.to_string()})),
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
    eval_externs(node, ctx, dispatcher, &NoExterns)
}

/// `eval` + externs registry for `call_extern` Expr resolution.
pub fn eval_externs<D: Dispatcher>(
    node: &Node,
    ctx: Value,
    dispatcher: &D,
    externs: &dyn Externs,
) -> Result<Value, EvalError> {
    let storage = MemoryCtx::new(ctx);
    eval_with_storage_externs(node, &storage, dispatcher, externs)?;
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

/// Evaluate an `Expr` against a context value, returning the resolved JSON
/// value. Externs-less compat wrapper — `call_extern` raises `ExternError`.
pub fn eval_expr(expr: &Expr, ctx: &Value) -> Result<Value, EvalError> {
    eval_expr_with_externs(expr, ctx, &NoExterns)
}

/// `eval_expr` + externs registry for `call_extern` Expr resolution.
pub fn eval_expr_with_externs(
    expr: &Expr,
    ctx: &Value,
    externs: &dyn Externs,
) -> Result<Value, EvalError> {
    let ev = |e: &Expr| eval_expr_with_externs(e, ctx, externs);
    match expr {
        Expr::Lit { value } => Ok(value.clone()),
        Expr::Path { at } => read_path(at, ctx),
        Expr::Eq { lhs, rhs } => Ok(Value::Bool(ev(lhs)? == ev(rhs)?)),
        Expr::Ne { lhs, rhs } => Ok(Value::Bool(ev(lhs)? != ev(rhs)?)),
        Expr::Lt { lhs, rhs } => ord_cmp(&ev(lhs)?, &ev(rhs)?, |o| o.is_lt()),
        Expr::Lte { lhs, rhs } => ord_cmp(&ev(lhs)?, &ev(rhs)?, |o| o.is_le()),
        Expr::Gt { lhs, rhs } => ord_cmp(&ev(lhs)?, &ev(rhs)?, |o| o.is_gt()),
        Expr::Gte { lhs, rhs } => ord_cmp(&ev(lhs)?, &ev(rhs)?, |o| o.is_ge()),
        Expr::Not { arg } => Ok(Value::Bool(!is_truthy(&ev(arg)?))),
        Expr::And { args } => {
            for a in args {
                if !is_truthy(&ev(a)?) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        Expr::Or { args } => {
            for a in args {
                if is_truthy(&ev(a)?) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        Expr::Exists { arg } => match ev(arg) {
            Ok(Value::Null) => Ok(Value::Bool(false)),
            Ok(_) => Ok(Value::Bool(true)),
            // canonical: a path to a missing key reads as nil → exists=false
            Err(EvalError::PathNotFound(_)) => Ok(Value::Bool(false)),
            Err(e) => Err(e),
        },
        Expr::Add { lhs, rhs } => num_arith(&ev(lhs)?, &ev(rhs)?, "add", |a, b| Some(a + b)),
        Expr::Sub { lhs, rhs } => num_arith(&ev(lhs)?, &ev(rhs)?, "sub", |a, b| Some(a - b)),
        Expr::Mul { lhs, rhs } => num_arith(&ev(lhs)?, &ev(rhs)?, "mul", |a, b| Some(a * b)),
        Expr::Div { lhs, rhs } => num_arith(&ev(lhs)?, &ev(rhs)?, "div", |a, b| {
            if b == 0.0 {
                None
            } else {
                Some(a / b)
            }
        }),
        // Lua `%` semantics (canonical): a - floor(a/b)*b, sign follows rhs.
        Expr::Mod { lhs, rhs } => num_arith(&ev(lhs)?, &ev(rhs)?, "mod", |a, b| {
            if b == 0.0 {
                None
            } else {
                Some(a - (a / b).floor() * b)
            }
        }),
        Expr::Len { arg } => {
            let v = ev(arg)?;
            let n = match &v {
                Value::Array(a) => a.len(),
                Value::String(s) => s.chars().count(),
                Value::Object(o) => o.len(),
                other => {
                    return Err(EvalError::DispatcherError {
                        ref_: "expr.len".into(),
                        msg: format!("len: unsupported type {other:?}"),
                    })
                }
            };
            Ok(Value::Number(serde_json::Number::from(n as u64)))
        }
        Expr::In { needle, haystack } => {
            let n = ev(needle)?;
            let h = ev(haystack)?;
            match h {
                Value::Array(a) => Ok(Value::Bool(a.iter().any(|e| e == &n))),
                other => Err(EvalError::DispatcherError {
                    ref_: "expr.in".into(),
                    msg: format!("in: haystack must be array, got {other:?}"),
                }),
            }
        }
        Expr::CallExtern { ref_, args } => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(ev(a)?);
            }
            externs.call(ref_, &vals)
        }
    }
}

/// Coerce a JSON value to f64 for numeric ops. Bool / null / non-number raise.
fn to_f64(v: &Value, op: &str) -> Result<f64, EvalError> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| EvalError::DispatcherError {
            ref_: format!("expr.{op}"),
            msg: format!("non-f64-representable number: {n}"),
        }),
        other => Err(EvalError::DispatcherError {
            ref_: format!("expr.{op}"),
            msg: format!("expected number, got {other:?}"),
        }),
    }
}

/// Ordering comparison over two evaluated values. Mirrors canonical Lua
/// `< / <= / > / >=`: both numbers (f64) or both strings (lexicographic
/// byte order, same as Lua's string comparison for UTF-8); anything else
/// raises.
fn ord_cmp<F>(lv: &Value, rv: &Value, f: F) -> Result<Value, EvalError>
where
    F: Fn(std::cmp::Ordering) -> bool,
{
    let ord = match (lv, rv) {
        (Value::Number(_), Value::Number(_)) => {
            let l = to_f64(lv, "cmp")?;
            let r = to_f64(rv, "cmp")?;
            l.partial_cmp(&r)
                .ok_or_else(|| EvalError::DispatcherError {
                    ref_: "expr.cmp".into(),
                    msg: "non-comparable numbers (NaN)".into(),
                })?
        }
        (Value::String(l), Value::String(r)) => l.cmp(r),
        (l, r) => {
            return Err(EvalError::DispatcherError {
                ref_: "expr.cmp".into(),
                msg: format!("cmp: both sides must be numbers or strings, got {l:?} vs {r:?}"),
            })
        }
    };
    Ok(Value::Bool(f(ord)))
}

fn num_arith<F>(lv: &Value, rv: &Value, op: &str, f: F) -> Result<Value, EvalError>
where
    F: Fn(f64, f64) -> Option<f64>,
{
    let l = to_f64(lv, op)?;
    let r = to_f64(rv, op)?;
    let result = f(l, r).ok_or_else(|| EvalError::DispatcherError {
        ref_: format!("expr.{op}"),
        msg: "arithmetic failure (e.g. division by zero)".into(),
    })?;
    let n = serde_json::Number::from_f64(result).ok_or_else(|| EvalError::DispatcherError {
        ref_: format!("expr.{op}"),
        msg: format!("result not f64-representable: {result}"),
    })?;
    Ok(Value::Number(n))
}

// ──────────────────────────────────────────────────────────────────────────
// Path helpers — `$.a.b.c` dot form, plus RFC 9535 (JSONPath) style bracket
// notation (`$.a["p.md"]`, `$["x.y"]`) for keys that contain a literal dot.
// No array index support (MVP scope).
// ──────────────────────────────────────────────────────────────────────────

/// Read a path from a JSON value.
///
/// Supports the simple dot form `$.a.b.c`, plus RFC 9535-style bracket
/// notation for object keys that contain a literal `.`: `$.a["p.md"]` reads
/// `ctx.a["p.md"]`, and `$["x.y"]` reads `ctx["x.y"]`. Bracket segments may
/// be chained directly (`$.a["x"]["y"]`) or followed by a dot segment
/// (`$["x.y"].inner`). Bracket keys support no escaping — a literal `"` in
/// a key cannot be represented.
///
/// Paths without `[` take the original dot-split code path unchanged
/// (no behavioural change for existing callers).
pub fn read_path(path: &str, ctx: &Value) -> Result<Value, EvalError> {
    let trimmed = strip_path_prefix(path)?;
    if trimmed.is_empty() {
        return Ok(ctx.clone());
    }
    let mut cur = ctx;
    if trimmed.contains('[') {
        let segments = parse_path_segments(trimmed)?;
        for key in &segments {
            cur = cur
                .get(key.as_str())
                .ok_or_else(|| EvalError::PathNotFound(path.to_string()))?;
        }
    } else {
        for key in trimmed.split('.') {
            cur = cur
                .get(key)
                .ok_or_else(|| EvalError::PathNotFound(path.to_string()))?;
        }
    }
    Ok(cur.clone())
}

/// Write a value at the path location inside ctx, returning the updated ctx.
/// `out` must be a `Path` Expr.
///
/// Accepts the same dot form and RFC 9535-style bracket notation as
/// [`read_path`] (see its docs for syntax + examples). Intermediate objects
/// along the path are created automatically, mirroring the existing
/// dot-form behaviour.
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
    let keys: Vec<String> = if trimmed.contains('[') {
        parse_path_segments(trimmed)?
    } else {
        trimmed
            .split('.')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    };
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

/// Parse a (prefix-stripped, non-empty) path string containing at least one
/// `[` into its object-key segments. Supports:
///
/// - plain segment: any run of chars excluding `.` and `[`, non-empty.
/// - bracket segment: `["<name>"]`, where `<name>` is one or more chars
///   excluding `"` (no escape support — a key containing `"` is rejected).
/// - plain segments are `.`-separated; a bracket segment may follow
///   directly after the previous segment (`a["x"]`) or after a `.`
///   (`a.["x"]`), and a bracket segment may itself be followed directly by
///   another bracket (`a["x"]["y"]`) or by a `.` before the next plain
///   segment (`a["x"].b`).
///
/// Any malformed sequence (unterminated bracket, missing quote, empty key,
/// empty segment, bracket directly followed by an unseparated plain
/// segment, ...) raises `EvalError::InvalidPath` — this parser never
/// silently misparses.
fn parse_path_segments(trimmed: &str) -> Result<Vec<String>, EvalError> {
    fn invalid(trimmed: &str, reason: &str) -> EvalError {
        EvalError::InvalidPath(format!("{reason}: {trimmed}"))
    }

    let bytes = trimmed.as_bytes();
    let len = bytes.len();
    let mut segments = Vec::new();
    let mut i = 0usize;
    // true at path start and immediately after a `.`: the next byte must
    // begin a new segment (plain or bracket), not another `.` or EOF.
    let mut expect_segment_start = true;

    while i < len {
        match bytes[i] {
            b'[' => {
                if i + 1 >= len || bytes[i + 1] != b'"' {
                    return Err(invalid(trimmed, "expected '\"' after '['"));
                }
                let name_start = i + 2;
                let mut j = name_start;
                while j < len && bytes[j] != b'"' {
                    j += 1;
                }
                if j >= len {
                    return Err(invalid(trimmed, "unterminated bracket segment"));
                }
                let name = &trimmed[name_start..j];
                if name.is_empty() {
                    return Err(invalid(trimmed, "empty bracket key"));
                }
                if j + 1 >= len || bytes[j + 1] != b']' {
                    return Err(invalid(trimmed, "missing closing ']' after key"));
                }
                segments.push(name.to_string());
                i = j + 2;
                expect_segment_start = false;
                // Only `.` or another `[` (or EOF) may directly follow a
                // bracket segment — a bare plain-segment continuation
                // (`a["x"]b`) is ambiguous and rejected.
                if i < len && bytes[i] != b'.' && bytes[i] != b'[' {
                    return Err(invalid(
                        trimmed,
                        "expected '.' or '[' after bracket segment",
                    ));
                }
            }
            b'.' => {
                if expect_segment_start {
                    return Err(invalid(trimmed, "empty path segment"));
                }
                i += 1;
                expect_segment_start = true;
                if i >= len {
                    return Err(invalid(trimmed, "empty path segment"));
                }
            }
            _ => {
                let start = i;
                while i < len && bytes[i] != b'.' && bytes[i] != b'[' {
                    i += 1;
                }
                segments.push(trimmed[start..i].to_string());
                expect_segment_start = false;
            }
        }
    }

    if expect_segment_start {
        return Err(invalid(trimmed, "empty path segment"));
    }

    Ok(segments)
}

fn write_path_recursive(node: &mut Value, keys: &[String], value: Value) {
    if keys.is_empty() {
        *node = value;
        return;
    }
    if !node.is_object() {
        *node = Value::Object(serde_json::Map::new());
    }
    let obj = node.as_object_mut().expect("just initialised as object");
    let key = &keys[0];
    if keys.len() == 1 {
        obj.insert(key.clone(), value);
    } else {
        let entry = obj
            .entry(key.clone())
            .or_insert(Value::Object(serde_json::Map::new()));
        write_path_recursive(entry, &keys[1..], value);
    }
}
