#![deny(unsafe_code)]
#![warn(missing_docs)]
//! flow.ir Pure Rust schema + sync interpreter.
//!
//! Node kinds (Step / Seq / Branch / Fanout / Loop / Try / Let) + Expr ops
//! (canonical wire format — comparison / boolean / existence / arithmetic /
//! aggregate / `call_extern`) + sync `eval` + `Dispatcher` trait + `Externs`
//! registry + typed [`Path`] read/write (see [`Path`] for the full path
//! syntax + uniform malformed-path rejection rules — the single authority,
//! rather than restating them here).
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

mod path;
pub use path::{Path, PathParseError};

// ──────────────────────────────────────────────────────────────────────────
// IR: 7 Node kinds + 20 Expr ops
// ──────────────────────────────────────────────────────────────────────────

/// flow.ir Node kind.
///
/// Discriminated with `kind` tag, `deny_unknown_fields` (open=false),
/// `rename_all = "snake_case"`. Covers the 7 supported kinds: `Step` / `Seq`
/// / `Branch` / `Fanout` (canonical schema の `fanout` Node、 4 join mode) /
/// `Loop` / `Try` / `Let` (canonical `let` — see the [`Node::Let`] variant
/// for the v0.3.0 rename from `Assign` and the accompanying `at` field
/// shape change). Additional kinds may be added in future versions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "snake_case")]
pub enum Node {
    /// `Step` — dispatch a referenced operation with `in` input, write result to `out`.
    Step {
        /// Dispatcher key (wire field `ref`), resolved via [`Dispatcher::dispatch`].
        #[serde(rename = "ref")]
        ref_: String,
        /// Input `Expr`, evaluated against the ctx snapshot before dispatch (wire field `in`).
        #[serde(rename = "in")]
        in_: Expr,
        /// `Path` `Expr` the dispatcher's output is written to.
        out: Expr,
    },
    /// `Seq` — evaluate children in order, threading the context value through.
    Seq {
        /// Child nodes, evaluated in order.
        children: Vec<Node>,
    },
    /// `Branch` — eval `cond`; if `true` run `then`, else run `else`.
    Branch {
        /// Condition `Expr`; must evaluate to a JSON boolean.
        cond: Expr,
        /// Branch taken when `cond` is `true` (wire field `then`).
        #[serde(rename = "then")]
        then_: Box<Node>,
        /// Branch taken when `cond` is `false` (wire field `else`).
        #[serde(rename = "else")]
        else_: Box<Node>,
    },
    /// `Fanout` — eval `items` to an array, run `body` per item against a
    /// branch-local ctx (caller ctx + item written to `bind`), join results
    /// per `join` mode into `out`. Async parallel runner uses
    /// `futures::future::{try_join_all|select_ok|join_all}` (executor-agnostic).
    Fanout {
        /// `Expr` evaluated to a JSON array; one branch runs per element.
        items: Expr,
        /// `Path` `Expr` each branch's item is written to before running `body`.
        bind: Expr,
        /// Node run once per `items` element, against a disjoint branch ctx.
        body: Box<Node>,
        /// How per-branch results are combined into `out`.
        join: JoinMode,
        /// `Path` `Expr` the joined result is written to.
        out: Expr,
    },
    /// `Loop` — counter を 0 から、 `cond` が truthy かつ `counter < max` の間
    /// `body` を eval。 各 iter 後 counter を increment して `counter` path に書く。
    /// VerdictLoop 等の retry/poll パターン primitive (canonical schema 整合)。
    Loop {
        /// `Path` `Expr` the iteration counter is written to (starts at `0`).
        counter: Expr,
        /// Condition re-evaluated before each iteration; loop stops once falsy.
        cond: Expr,
        /// Node evaluated once per iteration.
        body: Box<Node>,
        /// Hard iteration cap (loop stops once `counter >= max`, regardless of `cond`).
        max: u32,
    },
    /// `Try` — `body` を eval、 raise した場合 `catch` を eval。
    /// `err_at` が Some なら catch 開始前に error message を ctx に書く。
    Try {
        /// Node evaluated first; failures trigger a ctx rollback + `catch`.
        body: Box<Node>,
        /// Node evaluated when `body` raises.
        catch: Box<Node>,
        /// Optional `Path` `Expr` the error message is written to before `catch` runs.
        #[serde(default)]
        err_at: Option<Expr>,
    },
    /// `Let` — pure transform Node (canonical `let`). `value` Expr を ctx
    /// snapshot 上で評価し、 結果を `at` ([`Path`]) に write する。 dispatcher
    /// 不要、 副作用は `CtxStorage.write` 1 回のみ。 `Seq` の中で Step 間の
    /// Adhoc update 表現に使う (= IR primitive、 Command 履歴は CtxStorage の
    /// write hook 経由で取得)。
    ///
    /// **v0.3.0 rename (breaking):** this variant used to be `Node::Assign`
    /// with `at: Expr` (a `Path`-wrapping `Expr`). It now matches the
    /// canonical `flow.ir` schema's `let` node: the wire tag is `"let"` and
    /// `at` is a bare [`Path`], serialized as a plain path string
    /// (`"ctx.foo"`) rather than a `Path` `Expr` object. The canonical
    /// write-side prefix is `ctx.`; the parser accepts both `$` and `ctx`
    /// (root-token distinction is delegated to the caller — see [`Path`]).
    Let {
        /// [`Path`] the evaluated `value` is written to.
        at: Path,
        /// `Expr` evaluated against the ctx snapshot.
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
    Path {
        /// The parsed context path — see [`Path`] for syntax + rejection
        /// rules. Deserialized (and syntax-validated) once, at parse time.
        at: Path,
    },
    /// `Lit` — literal JSON value.
    Lit {
        /// The literal value, returned as-is on evaluation.
        value: Value,
    },
    /// `Eq` — boolean equality of two sub-expressions. Numbers compare by
    /// f64 value (`5 == 5.0` is `true`), matching the ordering ops' numeric
    /// coercion; integers above 2^53 may lose precision (same caveat as
    /// `Lt`/`Lte`/`Gt`/`Gte`).
    Eq {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Ne` — boolean inequality. Same numeric coercion as `Eq` (`5 != 5.0`
    /// is `false`); precision caveat above 2^53 shared with ordering ops.
    Ne {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Lt` — `lhs < rhs`. Both numbers (f64) or both strings (lexicographic),
    /// mirroring canonical Lua `<` semantics. Mixed / other types raise.
    Lt {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Lte` — `lhs <= rhs` (canonical wire tag `lte`).
    Lte {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Gt` — `lhs > rhs`.
    Gt {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Gte` — `lhs >= rhs` (canonical wire tag `gte`).
    Gte {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Not` — boolean negation of `arg` (truthy-based; null/false → true).
    Not {
        /// Operand negated by truthiness.
        arg: Box<Expr>,
    },
    /// `And` — variadic boolean conjunction (short-circuit). Empty list → true.
    And {
        /// Operands evaluated left-to-right until one is falsy.
        args: Vec<Expr>,
    },
    /// `Or` — variadic boolean disjunction (short-circuit). Empty list → false.
    Or {
        /// Operands evaluated left-to-right until one is truthy.
        args: Vec<Expr>,
    },
    /// `Exists` — evaluate `arg`; `true` iff it resolves to a non-null value.
    /// A `Path` arg that raises `PathNotFound` yields `false` (canonical
    /// `arg ~= nil` semantics — JSON null maps to Lua nil).
    Exists {
        /// Operand whose presence (non-null, resolvable) is tested.
        arg: Box<Expr>,
    },
    /// `Add` — numeric `lhs + rhs` (f64).
    Add {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Sub` — numeric `lhs - rhs`.
    Sub {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Mul` — numeric `lhs * rhs`.
    Mul {
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// `Div` — numeric `lhs / rhs`. Division by zero raises `ArithError`.
    Div {
        /// Left-hand operand (dividend).
        lhs: Box<Expr>,
        /// Right-hand operand (divisor).
        rhs: Box<Expr>,
    },
    /// `Mod` — numeric `lhs % rhs` (Lua `%` semantics: result takes the sign
    /// of `rhs`). Modulo by zero raises `ArithError`.
    Mod {
        /// Left-hand operand (dividend).
        lhs: Box<Expr>,
        /// Right-hand operand (divisor).
        rhs: Box<Expr>,
    },
    /// `Len` — length of `arg`: array → element count, string → char count,
    /// object → key count. Other types raise `TypeError`.
    Len {
        /// Operand whose length is computed.
        arg: Box<Expr>,
    },
    /// `In` — `true` if `needle` equals any element of `haystack` (which must
    /// evaluate to an array). Rust-side extension (not in canonical schema).
    /// Element equality uses the same numeric coercion as `Eq` (`5 == 5.0`);
    /// precision caveat above 2^53 shared with ordering ops.
    In {
        /// Value tested for membership.
        needle: Box<Expr>,
        /// Operand that must evaluate to a JSON array.
        haystack: Box<Expr>,
    },
    /// `CallExtern` — value-shape Hatch: resolve a host-injected pure function
    /// by opaque key via the `Externs` registry, apply it to evaluated args,
    /// return the value. The registered function MUST be pure (no side
    /// effects, no flow control) — see canonical `doc/ir.md §call_extern`.
    CallExtern {
        /// Extern registry key (wire field `ref`), resolved via [`Externs::call`].
        #[serde(rename = "ref")]
        ref_: String,
        /// Argument expressions, evaluated before the extern call.
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
    /// Resolve `ref_` against `input`, returning the step's raw output value.
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
///
/// `#[non_exhaustive]`: new variants may be added in a minor release: match
/// with a wildcard arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EvalError {
    /// A `Path` read did not resolve — the requested key is missing from ctx.
    #[error("path not found: {0}")]
    PathNotFound(String),
    /// A `Path` string is malformed — see [`Path`] for the full syntax +
    /// rejection rules. Raised by the `read_path` / `write_path` compat
    /// wrappers when their `&str` argument fails to parse; never raised by
    /// [`Path::read`] / [`Path::write`] themselves (an already-parsed
    /// `Path` cannot represent malformed syntax).
    #[error("invalid path syntax: {0}")]
    InvalidPath(String),
    /// `Branch.cond` evaluated to a non-boolean value.
    #[error("branch cond must be boolean, got: {0}")]
    NonBoolCond(Value),
    /// An expression or node received a value of the wrong type (e.g. a
    /// `Len`/`In`/comparison/arithmetic operand of the wrong JSON type, or a
    /// `Fanout.items` result that did not evaluate to an array).
    #[error("type error in '{op}': {msg}")]
    TypeError {
        /// The op (or synthetic label, e.g. `"fanout.any"`) that raised.
        op: String,
        /// Human-readable description of the type mismatch.
        msg: String,
    },
    /// An arithmetic operation failed (division/modulo by zero, or a
    /// numeric result/operand that cannot be represented as `f64`).
    #[error("arithmetic error in '{op}': {msg}")]
    ArithError {
        /// The op (e.g. `"div"`, `"mod"`, `"cmp"`) that raised.
        op: String,
        /// Human-readable description of the arithmetic failure.
        msg: String,
    },
    /// The [`Dispatcher`] returned an error for the given `Step.ref`.
    #[error("dispatcher error for ref '{ref_}': {msg}")]
    DispatcherError {
        /// The `Step.ref` that raised.
        ref_: String,
        /// The dispatcher's error message.
        msg: String,
    },
    /// The [`Externs`] registry raised for the given `call_extern.ref` (e.g.
    /// unregistered ref, or the extern fn itself returned an error).
    #[error("extern error for ref '{ref_}': {msg}")]
    ExternError {
        /// The `call_extern.ref` that raised.
        ref_: String,
        /// The extern fn's error message.
        msg: String,
    },
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
///     let x = args
///         .first()
///         .and_then(|v| v.as_f64())
///         .ok_or_else(|| EvalError::ExternError {
///             ref_: "math.sqrt".into(),
///             msg: "expected number".into(),
///         })?;
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
        let parsed: Path = path
            .parse()
            .map_err(|e: PathParseError| EvalError::InvalidPath(e.to_string()))?;
        let mut guard = self.inner.lock().expect("ctx mutex poisoned");
        parsed.write(&mut guard, value)
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

/// Extract the already-parsed [`Path`] out of a `Path` `Expr`, or
/// `InvalidPath` if `expr` is some other `Expr` variant. No parsing happens
/// here — the `Path` was parsed once, at deserialize (or `Path::from_str`)
/// time.
fn path_of(expr: &Expr) -> Result<&Path, EvalError> {
    match expr {
        Expr::Path { at } => Ok(at),
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
            ctx.write(&path_of(out)?.to_string(), output)
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
                    return Err(EvalError::TypeError {
                        op: "fanout.items".into(),
                        msg: format!("expected array, got {other:?}"),
                    })
                }
            };
            let joined =
                fanout_eval_sync(bind, body, *join, &snap, items_arr, dispatcher, externs)?;
            ctx.write(&path_of(out)?.to_string(), joined)
        }
        Node::Loop {
            counter,
            cond,
            body,
            max,
        } => {
            let counter_path = path_of(counter)?.to_string();
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
                eval_with_storage_externs(body, ctx, dispatcher, externs)?;
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
            // body 失敗時の rollback 用 snapshot
            let snap_before = ctx.snapshot();
            match eval_with_storage_externs(body, ctx, dispatcher, externs) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // body の途中 write を破棄 (Try semantic: rollback)
                    ctx.replace(snap_before);
                    if let Some(at) = err_at {
                        ctx.write(&path_of(at)?.to_string(), Value::String(e.to_string()))?;
                    }
                    eval_with_storage_externs(catch, ctx, dispatcher, externs)
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
            // Promise.any parity: zero items can never produce a winner, so
            // (unlike All/AllSettled, whose empty-array result shape is
            // still meaningful) this raises rather than returning `[]`.
            if items_arr.is_empty() {
                return Err(EvalError::TypeError {
                    op: "fanout.any".into(),
                    msg: "requires at least one item".into(),
                });
            }
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
            Ok(winner.expect("non-empty items_arr always assigns a winner or returns an error"))
        }
        JoinMode::Race => {
            // Same rationale as Any: zero branches means there is nothing to
            // race, so this raises rather than returning `[]`.
            let Some(first) = items_arr.into_iter().next() else {
                return Err(EvalError::TypeError {
                    op: "fanout.race".into(),
                    msg: "requires at least one item".into(),
                });
            };
            let branch_ctx = write_path(bind, base_snap.clone(), first)?;
            let storage = MemoryCtx::new(branch_ctx);
            eval_with_storage_externs(body, &storage, dispatcher, externs)?;
            Ok(storage.snapshot())
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
        // `at` is an already-parsed `Path` — no re-parse in this hot path.
        Expr::Path { at } => at.read(ctx).cloned(),
        Expr::Eq { lhs, rhs } => Ok(Value::Bool(json_eq(&ev(lhs)?, &ev(rhs)?))),
        Expr::Ne { lhs, rhs } => Ok(Value::Bool(!json_eq(&ev(lhs)?, &ev(rhs)?))),
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
                    return Err(EvalError::TypeError {
                        op: "expr.len".into(),
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
                Value::Array(a) => Ok(Value::Bool(a.iter().any(|e| json_eq(e, &n)))),
                other => Err(EvalError::TypeError {
                    op: "expr.in".into(),
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

/// Deep equality with Lua-parity numeric coercion: numbers compare by
/// f64 value (so `5 == 5.0`), matching the coercion Lt/Lte/Gt/Gte use.
/// Integers above 2^53 may lose precision — same caveat as ordering ops.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(na), Value::Number(nb)) => match (na.as_f64(), nb.as_f64()) {
            (Some(fa), Some(fb)) => fa == fb,
            // non-f64-representable (e.g. integers beyond 2^53): fall back
            // to exact comparison rather than a lossy f64 round-trip.
            _ => na == nb,
        },
        (Value::Array(aa), Value::Array(ab)) => {
            aa.len() == ab.len() && aa.iter().zip(ab.iter()).all(|(x, y)| json_eq(x, y))
        }
        (Value::Object(oa), Value::Object(ob)) => {
            oa.len() == ob.len()
                && oa
                    .iter()
                    .all(|(k, v)| ob.get(k).is_some_and(|ov| json_eq(v, ov)))
        }
        _ => a == b,
    }
}

/// Coerce a JSON value to f64 for numeric ops. A non-`Number` value is a
/// `TypeError` (wrong JSON type); a `Number` that itself cannot be
/// represented as `f64` (e.g. an integer beyond `f64`'s exact range) is an
/// `ArithError` (right type, unrepresentable value).
fn to_f64(v: &Value, op: &str) -> Result<f64, EvalError> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| EvalError::ArithError {
            op: op.into(),
            msg: format!("non-f64-representable number: {n}"),
        }),
        other => Err(EvalError::TypeError {
            op: op.into(),
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
            l.partial_cmp(&r).ok_or_else(|| EvalError::ArithError {
                op: "cmp".into(),
                msg: "non-comparable numbers (NaN)".into(),
            })?
        }
        (Value::String(l), Value::String(r)) => l.cmp(r),
        (l, r) => {
            return Err(EvalError::TypeError {
                op: "cmp".into(),
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
    let result = f(l, r).ok_or_else(|| EvalError::ArithError {
        op: op.into(),
        msg: "arithmetic failure (e.g. division by zero)".into(),
    })?;
    let n = serde_json::Number::from_f64(result).ok_or_else(|| EvalError::ArithError {
        op: op.into(),
        msg: format!("result not f64-representable: {result}"),
    })?;
    Ok(Value::Number(n))
}

// ──────────────────────────────────────────────────────────────────────────
// Path compat wrappers — thin `&str` entry points over the typed `Path` in
// `path.rs`, which is the single authority for path syntax + rejection
// rules. See [`Path`] docs for the full syntax (dot form, RFC 9535-style
// bracket notation, uniform malformed-path rejections).
// ──────────────────────────────────────────────────────────────────────────

/// Parse `path` and read the value it resolves to inside `ctx`.
///
/// Thin wrapper: parses `path` via [`str::parse`] (surfacing a parse
/// failure as [`EvalError::InvalidPath`]) then delegates to [`Path::read`].
/// Callers holding an already-parsed `Path` (e.g. from an `Expr::Path`)
/// should call [`Path::read`] directly to avoid re-parsing.
pub fn read_path(path: &str, ctx: &Value) -> Result<Value, EvalError> {
    let parsed: Path = path
        .parse()
        .map_err(|e: PathParseError| EvalError::InvalidPath(e.to_string()))?;
    parsed.read(ctx).cloned()
}

/// Write a value at the path location inside ctx, returning the updated ctx.
/// `out` must be a `Path` Expr (its `at` field is an already-parsed `Path`,
/// so no re-parsing happens here — this is a thin wrapper around
/// [`Path::write`], adapted to the `Value`-in/`Value`-out shape the rest of
/// this crate's legacy (non-`CtxStorage`) API uses).
pub fn write_path(out: &Expr, ctx: Value, value: Value) -> Result<Value, EvalError> {
    let path = path_of(out)?;
    let mut root = ctx;
    path.write(&mut root, value)?;
    Ok(root)
}
