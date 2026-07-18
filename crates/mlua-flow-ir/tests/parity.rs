//! Sync/async parity test corpus for flow.ir `Node` evaluation.
//!
//! Builds a corpus of `Node` trees (wire-format via `serde_json::from_value`)
//! and evaluates each with both the sync (`flow-ir-core`) and async
//! (`mlua-flow-ir`) evaluators, using a single deterministic fixture
//! dispatcher that implements both `Dispatcher` and `AsyncDispatcher` with
//! identical pure logic. Parity holds for all 7 `Node` kinds and for
//! `Fanout::All` / empty-items `Fanout` (any join mode). The two documented
//! exceptions (`Fanout::Race` / `Fanout::Any` with more than one item) are
//! covered separately below with divergence-documenting assertions instead
//! of parity assertions (see `crates/mlua-flow-ir/src/lib.rs` crate-root doc
//! comment "Sync/async divergence").

use async_trait::async_trait;
use mlua_flow_ir::{eval, eval_async, AsyncDispatcher, Dispatcher, EvalError, Node};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ──────────────────────────────────────────────────────────────────────────
// Fixture dispatcher — identical pure logic for sync + async
// ──────────────────────────────────────────────────────────────────────────

struct Fixture;

impl Dispatcher for Fixture {
    fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError> {
        fixture_logic(ref_, input)
    }
}

#[async_trait]
impl AsyncDispatcher for Fixture {
    async fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError> {
        fixture_logic(ref_, input)
    }
}

/// Pure fixture logic shared verbatim by the sync and async `Fixture`
/// dispatcher impls, so any observed sync/async divergence in the parity
/// corpus below can only come from the (hand-duplicated) `Node` eval match
/// arms, never from the dispatcher itself.
fn fixture_logic(ref_: &str, input: Value) -> Result<Value, EvalError> {
    match ref_ {
        "upper" => match input {
            Value::String(s) => Ok(Value::String(s.to_uppercase())),
            other => Ok(other),
        },
        "echo" => Ok(input),
        "fail" => Err(EvalError::DispatcherError {
            ref_: "fail".into(),
            msg: "intentional fixture failure".into(),
        }),
        other => Err(EvalError::DispatcherError {
            ref_: other.into(),
            msg: "unknown ref in fixture".into(),
        }),
    }
}

fn node_from(v: Value) -> Node {
    serde_json::from_value(v).expect("valid wire-format Node")
}

// ──────────────────────────────────────────────────────────────────────────
// Corpus
// ──────────────────────────────────────────────────────────────────────────

enum Expect {
    /// Both sides must succeed and be equal to each other. If `Some(v)` is
    /// given, the sync result must also equal `v` exactly (an extra
    /// correctness check beyond mere sync/async self-consistency).
    Ok(Option<Value>),
    /// Both sides must fail (message parity is checked; not a fixed
    /// string — see `try_rollback_err_at`'s dedicated structural checks for
    /// a case where the message text itself matters).
    Err,
}

struct Case {
    name: &'static str,
    node: Node,
    ctx: Value,
    expect: Expect,
}

fn branch_wire() -> Value {
    json!({
        "kind": "branch",
        "cond": {"op": "eq",
                 "lhs": {"op": "path", "at": "$.flag"},
                 "rhs": {"op": "lit", "value": true}},
        "then": {"kind": "step", "ref": "upper",
                 "in": {"op": "path", "at": "$.input"},
                 "out": {"op": "path", "at": "$.result"}},
        "else": {"kind": "step", "ref": "echo",
                 "in": {"op": "lit", "value": 1},
                 "out": {"op": "path", "at": "$.result"}},
    })
}

fn loop_wire(cond: Value, max: u32) -> Value {
    json!({
        "kind": "loop",
        "counter": {"op": "path", "at": "$.n"},
        "cond": cond,
        "body": {"kind": "let", "at": "ctx.sum",
                 "value": {"op": "add",
                           "lhs": {"op": "path", "at": "$.sum"},
                           "rhs": {"op": "lit", "value": 1}}},
        "max": max,
    })
}

fn fanout_wire(join: &str, items: Value) -> Value {
    json!({
        "kind": "fanout",
        "items": {"op": "lit", "value": items},
        "bind": {"op": "path", "at": "$.item"},
        "body": {"kind": "step", "ref": "upper",
                 "in": {"op": "path", "at": "$.item"},
                 "out": {"op": "path", "at": "$.item"}},
        "join": join,
        "out": {"op": "path", "at": "$.results"},
    })
}

fn build_corpus() -> Vec<Case> {
    vec![
        // 1a. Step — in/out path wiring
        Case {
            name: "step_wiring",
            node: node_from(json!({
                "kind": "step", "ref": "upper",
                "in": {"op": "path", "at": "$.input"},
                "out": {"op": "path", "at": "$.output"},
            })),
            ctx: json!({"input": "hi"}),
            expect: Expect::Ok(Some(json!({"input": "hi", "output": "HI"}))),
        },
        // 1b. Step — dispatcher error propagation
        Case {
            name: "step_dispatcher_error",
            node: node_from(json!({
                "kind": "step", "ref": "fail",
                "in": {"op": "lit", "value": null},
                "out": {"op": "path", "at": "$.x"},
            })),
            ctx: json!({}),
            expect: Expect::Err,
        },
        // 2. Seq — ordered writes
        Case {
            name: "seq_ordered_writes",
            node: node_from(json!({
                "kind": "seq",
                "children": [
                    {"kind": "step", "ref": "upper",
                     "in": {"op": "path", "at": "$.input"},
                     "out": {"op": "path", "at": "$.up"}},
                    {"kind": "let", "at": "ctx.count",
                     "value": {"op": "lit", "value": 1}},
                ],
            })),
            ctx: json!({"input": "w"}),
            expect: Expect::Ok(Some(json!({"input": "w", "up": "W", "count": 1}))),
        },
        // 3a. Branch — then
        Case {
            name: "branch_then",
            node: node_from(branch_wire()),
            ctx: json!({"flag": true, "input": "y"}),
            expect: Expect::Ok(Some(json!({"flag": true, "input": "y", "result": "Y"}))),
        },
        // 3b. Branch — else
        Case {
            name: "branch_else",
            node: node_from(branch_wire()),
            ctx: json!({"flag": false}),
            expect: Expect::Ok(Some(json!({"flag": false, "result": 1}))),
        },
        // 3c. Branch — NonBoolCond errors on both sides
        Case {
            name: "branch_non_bool_cond",
            node: node_from(json!({
                "kind": "branch",
                "cond": {"op": "lit", "value": "not bool"},
                "then": {"kind": "seq", "children": []},
                "else": {"kind": "seq", "children": []},
            })),
            ctx: json!({}),
            expect: Expect::Err,
        },
        // 4a. Loop — max cutoff (cond always true, 3 iterations)
        Case {
            name: "loop_max_cutoff",
            node: node_from(loop_wire(json!({"op": "lit", "value": true}), 3)),
            ctx: json!({"sum": 0}),
            expect: Expect::Ok(Some(json!({"sum": 3.0, "n": 3}))),
        },
        // 4b. Loop — cond false first (0 iterations, body never runs)
        Case {
            name: "loop_cond_false_first",
            node: node_from(loop_wire(json!({"op": "lit", "value": false}), 5)),
            ctx: json!({"sum": 0}),
            expect: Expect::Ok(Some(json!({"sum": 0, "n": 0}))),
        },
        // 5a. Try — success path (catch not taken)
        Case {
            name: "try_success_path",
            node: node_from(json!({
                "kind": "try",
                "body": {"kind": "let", "at": "ctx.x",
                         "value": {"op": "lit", "value": 1}},
                "catch": {"kind": "let", "at": "ctx.caught",
                          "value": {"op": "lit", "value": true}},
            })),
            ctx: json!({}),
            expect: Expect::Ok(Some(json!({"x": 1}))),
        },
        // 5b. Try — rollback on failure + err_at write. Parity-only here;
        // exact error text is asserted separately below (Step wraps the
        // dispatcher error message a second time, see fixture_logic's
        // "fail" ref + the `map_err` in both eval match arms).
        Case {
            name: "try_rollback_err_at",
            node: node_from(json!({
                "kind": "try",
                "body": {"kind": "seq", "children": [
                    {"kind": "let", "at": "ctx.partial",
                     "value": {"op": "lit", "value": "should-be-rolled-back"}},
                    {"kind": "step", "ref": "fail",
                     "in": {"op": "lit", "value": null},
                     "out": {"op": "path", "at": "$.never"}},
                ]},
                "catch": {"kind": "let", "at": "ctx.recovered",
                          "value": {"op": "lit", "value": true}},
                "err_at": {"op": "path", "at": "$.err"},
            })),
            ctx: json!({}),
            expect: Expect::Ok(None),
        },
        // 6. Assign
        Case {
            name: "assign_basic",
            node: node_from(json!({
                "kind": "let", "at": "ctx.x",
                "value": {"op": "lit", "value": 42},
            })),
            ctx: json!({}),
            expect: Expect::Ok(Some(json!({"x": 42}))),
        },
        // 7a. Fanout — All mode (result parity must hold)
        Case {
            name: "fanout_all_mode",
            node: node_from(fanout_wire("all", json!(["a", "b", "c"]))),
            ctx: json!({}),
            expect: Expect::Ok(Some(json!({
                "results": [{"item": "A"}, {"item": "B"}, {"item": "C"}],
            }))),
        },
        // 7b-e. Fanout — empty items, every JoinMode. All/AllSettled keep
        // their empty-array-is-a-meaningful-result shape; Any/Race now
        // raise (Promise.any/Promise.race parity — zero items can never
        // produce a winner) on both sides.
        Case {
            name: "fanout_empty_items_all",
            node: node_from(fanout_wire("all", json!([]))),
            ctx: json!({}),
            expect: Expect::Ok(Some(json!({"results": []}))),
        },
        Case {
            name: "fanout_empty_items_any",
            node: node_from(fanout_wire("any", json!([]))),
            ctx: json!({}),
            expect: Expect::Err,
        },
        Case {
            name: "fanout_empty_items_race",
            node: node_from(fanout_wire("race", json!([]))),
            ctx: json!({}),
            expect: Expect::Err,
        },
        Case {
            name: "fanout_empty_items_all_settled",
            node: node_from(fanout_wire("all_settled", json!([]))),
            ctx: json!({}),
            expect: Expect::Ok(Some(json!({"results": []}))),
        },
        // 8. Fanout — Race, single item. General Race parity does NOT hold
        // (sync only ever evaluates items[0]; async races every branch) —
        // but with exactly one item both sides evaluate the same single
        // branch, so parity holds here as a safe deterministic case.
        Case {
            name: "race_single_item",
            node: node_from(fanout_wire("race", json!(["only"]))),
            ctx: json!({}),
            expect: Expect::Ok(Some(json!({"results": {"item": "ONLY"}}))),
        },
        // 9. Fanout — Any, single item, the (only) item succeeds. General
        // Any parity does NOT hold once more than one item is present (see
        // the dedicated divergence test below) — this is the safe
        // deterministic single-item case.
        Case {
            name: "any_single_item_first_succeeds",
            node: node_from(fanout_wire("any", json!(["solo"]))),
            ctx: json!({}),
            expect: Expect::Ok(Some(json!({"results": {"item": "SOLO"}}))),
        },
        // 10. Nested composite: Seq[Branch[Step], Try[Loop[Assign]]]
        Case {
            name: "nested_composite",
            node: node_from(json!({
                "kind": "seq",
                "children": [
                    {"kind": "branch",
                     "cond": {"op": "eq",
                              "lhs": {"op": "path", "at": "$.flag"},
                              "rhs": {"op": "lit", "value": true}},
                     "then": {"kind": "step", "ref": "upper",
                              "in": {"op": "path", "at": "$.input"},
                              "out": {"op": "path", "at": "$.branched"}},
                     "else": {"kind": "let", "at": "ctx.branched",
                              "value": {"op": "lit", "value": "no"}}},
                    {"kind": "try",
                     "body": loop_wire(
                        json!({"op": "lt",
                               "lhs": {"op": "path", "at": "$.n"},
                               "rhs": {"op": "lit", "value": 3}}),
                        5,
                     ),
                     "catch": {"kind": "let", "at": "ctx.caught",
                               "value": {"op": "lit", "value": true}}},
                ],
            })),
            ctx: json!({"flag": true, "input": "go", "sum": 0}),
            expect: Expect::Ok(Some(json!({
                "flag": true, "input": "go", "sum": 3.0, "branched": "GO", "n": 3,
            }))),
        },
        // 11. Expr parity through Step in/out + Branch cond: eq/add, so the
        // numeric-coercion path is exercised identically on both evaluators
        // (both call the same shared `eval_expr_with_externs`, but the
        // surrounding Node match arms are hand-duplicated — this guards
        // against future drift there).
        Case {
            name: "expr_parity_eq_add",
            node: node_from(json!({
                "kind": "seq",
                "children": [
                    {"kind": "step", "ref": "echo",
                     "in": {"op": "add",
                            "lhs": {"op": "lit", "value": 2},
                            "rhs": {"op": "lit", "value": 3}},
                     "out": {"op": "path", "at": "$.sum_via_step"}},
                    {"kind": "branch",
                     "cond": {"op": "eq",
                              "lhs": {"op": "path", "at": "$.sum_via_step"},
                              "rhs": {"op": "lit", "value": 5}},
                     "then": {"kind": "let", "at": "ctx.route",
                              "value": {"op": "lit", "value": "via-eq-add"}},
                     "else": {"kind": "let", "at": "ctx.route",
                              "value": {"op": "lit", "value": "wrong"}}},
                ],
            })),
            ctx: json!({}),
            expect: Expect::Ok(Some(json!({"sum_via_step": 5.0, "route": "via-eq-add"}))),
        },
    ]
}

#[tokio::test]
async fn sync_async_parity_corpus() {
    let cases = build_corpus();
    let mut sync_results: HashMap<&'static str, Value> = HashMap::new();

    for case in &cases {
        let sync_result = eval(&case.node, case.ctx.clone(), &Fixture);
        let async_result = eval_async(&case.node, case.ctx.clone(), &Fixture).await;

        match &case.expect {
            Expect::Err => {
                let sync_err = sync_result.unwrap_err_named(case.name, "sync expected Err, got Ok");
                let async_err =
                    async_result.unwrap_err_named(case.name, "async expected Err, got Ok");
                assert_eq!(
                    sync_err.to_string(),
                    async_err.to_string(),
                    "[{}] sync/async error message diverged",
                    case.name,
                );
            }
            Expect::Ok(expected) => {
                let sync_val =
                    sync_result.unwrap_or_else(|e| panic!("[{}] sync eval failed: {e}", case.name));
                let async_val = async_result
                    .unwrap_or_else(|e| panic!("[{}] async eval failed: {e}", case.name));
                assert_eq!(
                    sync_val, async_val,
                    "[{}] sync/async result diverged",
                    case.name,
                );
                if let Some(expected) = expected {
                    assert_eq!(
                        &sync_val, expected,
                        "[{}] result did not match expected value",
                        case.name,
                    );
                }
                sync_results.insert(case.name, sync_val);
            }
        }
    }

    // Extra structural checks for try_rollback_err_at (exact error text not
    // asserted above — see the Case comment).
    let rollback = &sync_results["try_rollback_err_at"];
    assert!(
        rollback.get("partial").is_none(),
        "Try must roll back writes made before the failing Step: {rollback:?}"
    );
    assert!(
        rollback.get("never").is_none(),
        "the failing Step's `out` must never be written: {rollback:?}"
    );
    assert_eq!(rollback["recovered"], json!(true));
    assert!(
        rollback["err"].as_str().is_some_and(|s| !s.is_empty()),
        "err_at target must hold a non-empty error message: {rollback:?}"
    );
}

/// Small helper trait so the `Expect::Err` arm above reads as
/// `result.unwrap_err_named(name, msg)` instead of a raw `.expect(&format!(..))`.
trait UnwrapErrNamed<E> {
    fn unwrap_err_named(self, name: &str, msg: &str) -> E;
}

impl<T: std::fmt::Debug, E> UnwrapErrNamed<E> for Result<T, E> {
    fn unwrap_err_named(self, name: &str, msg: &str) -> E {
        match self {
            Ok(v) => panic!("[{name}] {msg}: got Ok({v:?})"),
            Err(e) => e,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Documented Fanout divergence: JoinMode::Any, > 1 item
//
// General Any parity does NOT hold once more than one item is present: sync
// short-circuits sequentially and never dispatches later items once one
// succeeds, while async launches every branch concurrently via
// `futures::future::select_ok` (see the crate-root doc comment "Sync/async
// divergence" in `crates/mlua-flow-ir/src/lib.rs`). This test does not
// assert value parity — it asserts (and documents) the dispatch-count
// divergence directly.
// ──────────────────────────────────────────────────────────────────────────

struct CountingDispatcher {
    counter: Arc<AtomicUsize>,
}

impl Dispatcher for CountingDispatcher {
    fn dispatch(&self, _ref_: &str, input: Value) -> Result<Value, EvalError> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(input)
    }
}

#[async_trait]
impl AsyncDispatcher for CountingDispatcher {
    async fn dispatch(&self, _ref_: &str, input: Value) -> Result<Value, EvalError> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        // Force a real suspend point so every branch future actually gets
        // polled (and thus dispatched) before `select_ok` can pick a
        // winner. Without this, a dispatcher that never suspends would let
        // `select_ok` short-circuit on item 0's very first poll too (same
        // as sync), masking the divergence this test exists to document.
        tokio::task::yield_now().await;
        Ok(input)
    }
}

#[tokio::test]
async fn fanout_any_multi_item_divergence_sync_short_circuits_async_dispatches_all() {
    let node = node_from(fanout_wire("any", json!([1, 2, 3])));

    let sync_counter = Arc::new(AtomicUsize::new(0));
    let sync_result = eval(
        &node,
        json!({}),
        &CountingDispatcher {
            counter: sync_counter.clone(),
        },
    )
    .unwrap();
    let sync_count = sync_counter.load(Ordering::SeqCst);

    let async_counter = Arc::new(AtomicUsize::new(0));
    let async_result = eval_async(
        &node,
        json!({}),
        &CountingDispatcher {
            counter: async_counter.clone(),
        },
    )
    .await
    .unwrap();
    let async_count = async_counter.load(Ordering::SeqCst);

    // Both sides still succeed with a single winning branch (Any
    // semantics), only the *number of dispatches en route* diverges.
    assert!(sync_result["results"].is_object());
    assert!(async_result["results"].is_object());

    // DIVERGENCE OBSERVED (intentional, documented in src/lib.rs): sync Any
    // short-circuits sequentially and stops dispatching after the first
    // success; async Any launches every branch concurrently, so every
    // item's dispatch is invoked before a winner is selected.
    assert!(
        sync_count <= async_count,
        "sync ({sync_count}) must dispatch no more than async ({async_count})"
    );
    assert_eq!(
        sync_count, 1,
        "sync Any short-circuits after the first success"
    );
    assert_eq!(
        async_count, 3,
        "async Any dispatches every branch concurrently before selecting a winner"
    );
}
