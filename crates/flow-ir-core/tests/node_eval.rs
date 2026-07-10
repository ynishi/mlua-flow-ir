//! Node-level semantics coverage (Step / Seq / Branch / Fanout / Loop / Try /
//! Assign) plus serde wire-format robustness (unknown-field handling,
//! round-trip stability, rename survival). `expr_ops.rs` covers `Expr` ops
//! only; this file is the Node-kind counterpart plus schema hardening.

use flow_ir_core::{eval, eval_with_storage, CtxStorage, Dispatcher, EvalError, MemoryCtx, Node};
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::HashSet;

// ──────────────────────────────────────────────────────────────────────────
// Fixture dispatcher
// ──────────────────────────────────────────────────────────────────────────

/// Fixture dispatcher used across Node-level tests. Defaults to echoing the
/// input value back unchanged (identity dispatch); any `ref_` listed in
/// `fail_refs` raises `EvalError::DispatcherError` instead. Tracks the total
/// number of `dispatch` calls (`calls()`) so tests can assert a body was (or
/// was never) reached.
struct FxDispatcher {
    count: Cell<u32>,
    fail_refs: HashSet<String>,
}

impl FxDispatcher {
    fn new() -> Self {
        Self {
            count: Cell::new(0),
            fail_refs: HashSet::new(),
        }
    }

    fn failing(refs: &[&str]) -> Self {
        Self {
            count: Cell::new(0),
            fail_refs: refs.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn calls(&self) -> u32 {
        self.count.get()
    }
}

impl Dispatcher for FxDispatcher {
    fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError> {
        self.count.set(self.count.get() + 1);
        if self.fail_refs.contains(ref_) {
            return Err(EvalError::DispatcherError {
                ref_: ref_.into(),
                msg: "fixture failure".into(),
            });
        }
        Ok(input)
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Seq
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn seq_children_execute_in_order_and_see_earlier_writes() {
    let node: Node = serde_json::from_value(json!({
        "kind": "seq",
        "children": [
            {"kind":"assign","at":{"op":"path","at":"$.a"},"value":{"op":"lit","value":1}},
            {"kind":"assign","at":{"op":"path","at":"$.b"},
             "value":{"op":"add","lhs":{"op":"path","at":"$.a"},"rhs":{"op":"lit","value":1}}}
        ]
    }))
    .unwrap();
    let out = eval(&node, json!({}), &FxDispatcher::new()).unwrap();
    assert_eq!(out, json!({"a": 1, "b": 2.0}));
}

#[test]
fn seq_empty_children_returns_ctx_unchanged() {
    let node: Node = serde_json::from_value(json!({"kind": "seq", "children": []})).unwrap();
    let ctx = json!({"x": 1});
    let out = eval(&node, ctx.clone(), &FxDispatcher::new()).unwrap();
    assert_eq!(out, ctx);
}

// ──────────────────────────────────────────────────────────────────────────
// Branch
// ──────────────────────────────────────────────────────────────────────────

fn branch_node(cond_value: Value) -> Node {
    serde_json::from_value(json!({
        "kind": "branch",
        "cond": {"op": "lit", "value": cond_value},
        "then": {"kind":"assign","at":{"op":"path","at":"$.r"},"value":{"op":"lit","value":"then"}},
        "else": {"kind":"assign","at":{"op":"path","at":"$.r"},"value":{"op":"lit","value":"else"}}
    }))
    .unwrap()
}

#[test]
fn branch_routes_to_then_on_true_cond() {
    let out = eval(&branch_node(json!(true)), json!({}), &FxDispatcher::new()).unwrap();
    assert_eq!(out, json!({"r": "then"}));
}

#[test]
fn branch_routes_to_else_on_false_cond() {
    let out = eval(&branch_node(json!(false)), json!({}), &FxDispatcher::new()).unwrap();
    assert_eq!(out, json!({"r": "else"}));
}

#[test]
fn branch_non_bool_cond_raises_non_bool_cond() {
    let err = eval(&branch_node(json!(1)), json!({}), &FxDispatcher::new()).unwrap_err();
    assert!(matches!(err, EvalError::NonBoolCond(_)), "{err:?}");
}

// ──────────────────────────────────────────────────────────────────────────
// Fanout
// ──────────────────────────────────────────────────────────────────────────

/// Fanout node whose body doubles (`* 10`) the bound `$.item` into
/// `$.doubled`, joined into `$.results` per `join`.
fn fanout_node(join: &str, items: Value) -> Node {
    fanout_with_body(
        join,
        items,
        json!({
            "kind": "assign",
            "at": {"op": "path", "at": "$.doubled"},
            "value": {"op": "mul", "lhs": {"op": "path", "at": "$.item"}, "rhs": {"op": "lit", "value": 10}}
        }),
    )
}

fn fanout_with_body(join: &str, items: Value, body: Value) -> Node {
    serde_json::from_value(json!({
        "kind": "fanout",
        "items": {"op": "lit", "value": items},
        "bind": {"op": "path", "at": "$.item"},
        "body": body,
        "join": join,
        "out": {"op": "path", "at": "$.results"}
    }))
    .unwrap()
}

/// Branch body: item == 2 dispatches the (failable) `boom` ref, else writes
/// `$.out = "ok"` directly. Used by the error-aggregation tests below.
fn branchy_body_json() -> Value {
    json!({
        "kind": "branch",
        "cond": {"op": "eq", "lhs": {"op": "path", "at": "$.item"}, "rhs": {"op": "lit", "value": 2}},
        "then": {"kind": "step", "ref": "boom", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.out"}},
        "else": {"kind": "assign", "at": {"op": "path", "at": "$.out"}, "value": {"op": "lit", "value": "ok"}}
    })
}

#[test]
fn fanout_all_mode_success_shape_is_array_of_per_branch_ctx() {
    let node = fanout_node("all", json!([1, 2, 3]));
    let out = eval(&node, json!({}), &FxDispatcher::new()).unwrap();
    assert_eq!(
        out["results"],
        json!([
            {"item": 1, "doubled": 10.0},
            {"item": 2, "doubled": 20.0},
            {"item": 3, "doubled": 30.0}
        ])
    );
}

#[test]
fn fanout_any_mode_success_shape_is_single_winner_ctx() {
    let node = fanout_node("any", json!([1, 2, 3]));
    let out = eval(&node, json!({}), &FxDispatcher::new()).unwrap();
    // Any: first non-raising branch's ctx wins — a single ctx *object*, not
    // an array (contrast with All/AllSettled below).
    assert_eq!(out["results"], json!({"item": 1, "doubled": 10.0}));
}

#[test]
fn fanout_race_mode_success_shape_is_first_item_ctx_only() {
    let node = fanout_node("race", json!([1, 2, 3]));
    let out = eval(&node, json!({}), &FxDispatcher::new()).unwrap();
    // Race (sync runner): only the first item is ever evaluated; its ctx is
    // the result regardless of how many items were in the array.
    assert_eq!(out["results"], json!({"item": 1, "doubled": 10.0}));
}

#[test]
fn fanout_all_settled_mode_success_shape_is_status_records() {
    let node = fanout_node("all_settled", json!([1, 2]));
    let out = eval(&node, json!({}), &FxDispatcher::new()).unwrap();
    assert_eq!(
        out["results"],
        json!([
            {"status": "fulfilled", "value": {"item": 1, "doubled": 10.0}},
            {"status": "fulfilled", "value": {"item": 2, "doubled": 20.0}}
        ])
    );
}

#[test]
fn fanout_empty_items_shapes() {
    // All / AllSettled: an empty `items` array is consistent with their
    // non-empty "array of per-branch results" shape — just an empty array.
    let all_out = eval(
        &fanout_node("all", json!([])),
        json!({}),
        &FxDispatcher::new(),
    )
    .unwrap();
    assert_eq!(all_out["results"], json!([]));

    let settled_out = eval(
        &fanout_node("all_settled", json!([])),
        json!({}),
        &FxDispatcher::new(),
    )
    .unwrap();
    assert_eq!(settled_out["results"], json!([]));

    // NOTE: Any / Race normally resolve to a *single ctx object* on success
    // (see fanout_any_mode_success_shape_is_single_winner_ctx /
    // fanout_race_mode_success_shape_is_first_item_ctx_only above). With an
    // empty `items` array there is no branch to produce that object, so the
    // implementation falls back to `Value::Array(vec![])` — an empty
    // *array*, a different shape family than the non-empty "winner ctx
    // object" case. This is an existing shape inconsistency in
    // `fanout_eval_sync`'s Any/Race arms; asserting the actual observed
    // behavior here, not proposing a fix (src is out of scope for this
    // test-only pass).
    let any_out = eval(
        &fanout_node("any", json!([])),
        json!({}),
        &FxDispatcher::new(),
    )
    .unwrap();
    assert_eq!(any_out["results"], json!([]));

    let race_out = eval(
        &fanout_node("race", json!([])),
        json!({}),
        &FxDispatcher::new(),
    )
    .unwrap();
    assert_eq!(race_out["results"], json!([]));
}

#[test]
fn fanout_non_array_items_errors() {
    let node = fanout_with_body(
        "all",
        json!("not-an-array"),
        json!({"kind": "assign", "at": {"op": "path", "at": "$.x"}, "value": {"op": "lit", "value": 1}}),
    );
    let err = eval(&node, json!({}), &FxDispatcher::new()).unwrap_err();
    assert!(
        matches!(err, EvalError::DispatcherError { ref ref_, .. } if ref_ == "fanout.items"),
        "{err:?}"
    );
}

#[test]
fn fanout_all_mode_one_failing_branch_errors() {
    let node = fanout_with_body("all", json!([1, 2]), branchy_body_json());
    let dispatcher = FxDispatcher::failing(&["boom"]);
    let err = eval(&node, json!({}), &dispatcher).unwrap_err();
    assert!(matches!(err, EvalError::DispatcherError { .. }), "{err:?}");
}

#[test]
fn fanout_any_mode_all_branches_failing_errors() {
    let body = json!({"kind": "step", "ref": "boom", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.out"}});
    let node = fanout_with_body("any", json!([1, 2]), body);
    let dispatcher = FxDispatcher::failing(&["boom"]);
    let err = eval(&node, json!({}), &dispatcher).unwrap_err();
    assert!(matches!(err, EvalError::DispatcherError { .. }), "{err:?}");
}

#[test]
fn fanout_any_mode_one_success_among_failures_succeeds() {
    // item 2 first (fails via "boom"), item 1 second (succeeds) — Any keeps
    // scanning past failures and returns the first winning ctx.
    let node = fanout_with_body("any", json!([2, 1]), branchy_body_json());
    let dispatcher = FxDispatcher::failing(&["boom"]);
    let out = eval(&node, json!({}), &dispatcher).unwrap();
    assert_eq!(out["results"], json!({"item": 1, "out": "ok"}));
}

// ──────────────────────────────────────────────────────────────────────────
// Loop
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn loop_max_cutoff_terminates() {
    let node: Node = serde_json::from_value(json!({
        "kind": "loop",
        "counter": {"op": "path", "at": "$.counter"},
        "cond": {"op": "lit", "value": true},
        "body": {"kind": "step", "ref": "tick", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.last"}},
        "max": 3
    }))
    .unwrap();
    let dispatcher = FxDispatcher::new();
    let out = eval(&node, json!({}), &dispatcher).unwrap();
    assert_eq!(dispatcher.calls(), 3);
    assert_eq!(out["counter"], json!(3));
}

#[test]
fn loop_cond_false_on_first_check_body_never_dispatched() {
    let node: Node = serde_json::from_value(json!({
        "kind": "loop",
        "counter": {"op": "path", "at": "$.counter"},
        "cond": {"op": "lit", "value": false},
        "body": {"kind": "step", "ref": "tick", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.last"}},
        "max": 5
    }))
    .unwrap();
    let dispatcher = FxDispatcher::new();
    let out = eval(&node, json!({}), &dispatcher).unwrap();
    assert_eq!(dispatcher.calls(), 0);
    // counter is written to 0 unconditionally before the first cond check.
    assert_eq!(out["counter"], json!(0));
    // body never ran, so $.last was never written.
    assert!(out.get("last").is_none());
}

#[test]
fn loop_ctx_writes_visible_across_iterations_and_cond_stops_the_loop() {
    let node: Node = serde_json::from_value(json!({
        "kind": "loop",
        "counter": {"op": "path", "at": "$.counter"},
        "cond": {"op": "lt", "lhs": {"op": "path", "at": "$.sum"}, "rhs": {"op": "lit", "value": 3}},
        "body": {"kind": "assign", "at": {"op": "path", "at": "$.sum"},
                 "value": {"op": "add", "lhs": {"op": "path", "at": "$.sum"}, "rhs": {"op": "lit", "value": 1}}},
        "max": 10
    }))
    .unwrap();
    // sum starts at 0; each iteration reads the previous iteration's write
    // (sum += 1) and stops once sum reaches 3 — well short of max=10, proving
    // writes propagate across iterations rather than each iteration seeing a
    // fresh/stale ctx.
    let out = eval(&node, json!({"sum": 0}), &FxDispatcher::new()).unwrap();
    assert_eq!(out["sum"], json!(3.0));
    assert_eq!(out["counter"], json!(3));
}

// ──────────────────────────────────────────────────────────────────────────
// Try
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn try_body_failure_rolls_back_ctx_to_pre_body_snapshot() {
    let node: Node = serde_json::from_value(json!({
        "kind": "try",
        "body": {
            "kind": "seq",
            "children": [
                {"kind": "assign", "at": {"op": "path", "at": "$.mark"}, "value": {"op": "lit", "value": "was-here"}},
                {"kind": "step", "ref": "boom", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.out"}}
            ]
        },
        "catch": {"kind": "assign", "at": {"op": "path", "at": "$.caught"}, "value": {"op": "lit", "value": true}}
    }))
    .unwrap();
    let dispatcher = FxDispatcher::failing(&["boom"]);
    let out = eval(&node, json!({}), &dispatcher).unwrap();
    assert!(
        out.get("mark").is_none(),
        "body write must be rolled back on failure: {out:?}"
    );
    assert_eq!(out["caught"], json!(true));
}

#[test]
fn try_err_at_writes_error_message_before_catch() {
    let node: Node = serde_json::from_value(json!({
        "kind": "try",
        "body": {"kind": "step", "ref": "boom", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.out"}},
        "catch": {"kind": "assign", "at": {"op": "path", "at": "$.caught"}, "value": {"op": "lit", "value": true}},
        "err_at": {"op": "path", "at": "$.err"}
    }))
    .unwrap();
    let dispatcher = FxDispatcher::failing(&["boom"]);
    let out = eval(&node, json!({}), &dispatcher).unwrap();
    assert_eq!(out["caught"], json!(true));
    let err_msg = out["err"]
        .as_str()
        .expect("err_at should write a string message");
    assert!(
        err_msg.contains("boom"),
        "expected dispatcher ref in error message, got: {err_msg}"
    );
}

#[test]
fn try_catch_raising_propagates() {
    let node: Node = serde_json::from_value(json!({
        "kind": "try",
        "body": {"kind": "step", "ref": "boom", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.out"}},
        "catch": {"kind": "step", "ref": "catch_boom", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.out2"}}
    }))
    .unwrap();
    let dispatcher = FxDispatcher::failing(&["boom", "catch_boom"]);
    let err = eval(&node, json!({}), &dispatcher).unwrap_err();
    assert!(
        matches!(err, EvalError::DispatcherError { ref ref_, .. } if ref_ == "catch_boom"),
        "{err:?}"
    );
}

#[test]
fn try_body_success_catch_not_executed() {
    let node: Node = serde_json::from_value(json!({
        "kind": "try",
        "body": {"kind": "assign", "at": {"op": "path", "at": "$.ok"}, "value": {"op": "lit", "value": true}},
        "catch": {"kind": "step", "ref": "should_not_run", "in": {"op": "lit", "value": null}, "out": {"op": "path", "at": "$.out"}}
    }))
    .unwrap();
    let dispatcher = FxDispatcher::new();
    let out = eval(&node, json!({}), &dispatcher).unwrap();
    assert_eq!(out, json!({"ok": true}));
    assert_eq!(dispatcher.calls(), 0);
}

// ──────────────────────────────────────────────────────────────────────────
// Assign
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn assign_writes_evaluated_expr_to_out_path() {
    let node: Node = serde_json::from_value(json!({
        "kind": "assign",
        "at": {"op": "path", "at": "$.out"},
        "value": {"op": "add", "lhs": {"op": "lit", "value": 1}, "rhs": {"op": "lit", "value": 2}}
    }))
    .unwrap();
    let out = eval(&node, json!({}), &FxDispatcher::new()).unwrap();
    assert_eq!(out, json!({"out": 3.0}));
}

// ──────────────────────────────────────────────────────────────────────────
// eval_with_storage / MemoryCtx
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn eval_with_storage_shared_memory_ctx_visible_after_eval() {
    let storage = MemoryCtx::new(json!({"input": "hi"}));
    let node: Node = serde_json::from_value(json!({
        "kind": "step",
        "ref": "noop",
        "in": {"op": "path", "at": "$.input"},
        "out": {"op": "path", "at": "$.output"}
    }))
    .unwrap();
    eval_with_storage(&node, &storage, &FxDispatcher::new()).unwrap();
    assert_eq!(storage.read("$.output").unwrap(), json!("hi"));
    assert_eq!(storage.snapshot(), json!({"input": "hi", "output": "hi"}));
}

#[test]
fn memory_ctx_snapshot_replace_round_trip() {
    let storage = MemoryCtx::new(json!({"a": 1}));
    let snap = storage.snapshot();
    assert_eq!(snap, json!({"a": 1}));

    storage.replace(json!({"b": 2}));
    assert_eq!(storage.snapshot(), json!({"b": 2}));

    // round-trip: replace back with the original snapshot
    storage.replace(snap.clone());
    assert_eq!(storage.snapshot(), snap);
}

// ──────────────────────────────────────────────────────────────────────────
// Serde robustness
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn expr_unknown_field_observed_behavior() {
    let res = serde_json::from_value::<flow_ir_core::Expr>(json!({
        "op": "lit",
        "value": 1,
        "bogus": 2
    }));
    // NOTE: the crate doc comment on `Expr` (and a well-known historical
    // serde caveat, serde-rs/serde#1600) warns that `deny_unknown_fields`
    // can be silently ignored for internally-tagged enums (`#[serde(tag =
    // "op")]`) deserialized via serde_json's `Content`-buffer path. That
    // limitation does NOT reproduce here with the pinned serde/serde_json
    // versions (see Cargo.lock) — `Content`-buffer deserialization of a
    // non-flattened internally-tagged variant does still run
    // `deny_unknown_fields`. OBSERVED: the unknown field IS rejected.
    assert!(res.is_err(), "{res:?}");
    let msg = res.unwrap_err().to_string();
    assert!(msg.contains("bogus"), "{msg}");
}

#[test]
fn node_unknown_field_observed_behavior() {
    let res = serde_json::from_value::<Node>(json!({
        "kind": "step",
        "ref": "x",
        "in": {"op": "lit", "value": 1},
        "out": {"op": "path", "at": "$.o"},
        "bogus": true
    }));
    // NOTE: same observation as expr_unknown_field_observed_behavior — Node
    // is also internally tagged (`#[serde(tag = "kind")]`) without
    // `flatten`, and `deny_unknown_fields` is enforced as documented.
    assert!(res.is_err(), "{res:?}");
    let msg = res.unwrap_err().to_string();
    assert!(msg.contains("bogus"), "{msg}");
}

#[test]
fn node_unknown_kind_tag_rejected() {
    let res = serde_json::from_value::<Node>(json!({
        "kind": "bogus_kind",
        "foo": 1
    }));
    assert!(res.is_err(), "{res:?}");
}

#[test]
fn try_absent_err_at_parses_as_none_and_serializes_explicit_null() {
    let node: Node = serde_json::from_value(json!({
        "kind": "try",
        "body": {"kind": "assign", "at": {"op": "path", "at": "$.a"}, "value": {"op": "lit", "value": 1}},
        "catch": {"kind": "assign", "at": {"op": "path", "at": "$.b"}, "value": {"op": "lit", "value": 2}}
    }))
    .unwrap();
    match &node {
        Node::Try { err_at, .. } => assert!(err_at.is_none()),
        other => panic!("expected Try, got {other:?}"),
    }
    let v = serde_json::to_value(&node).unwrap();
    // OBSERVED: `err_at` has no `skip_serializing_if`, so the default
    // `Option<T>` Serialize impl emits an explicit `"err_at": null` rather
    // than omitting the key.
    assert!(v.as_object().unwrap().contains_key("err_at"));
    assert_eq!(v["err_at"], json!(null));
}

#[test]
fn node_tree_full_roundtrip_all_kinds_stable_reserialization() {
    // One Node tree exercising all 7 Node kinds (Try > Seq > {Step, Branch,
    // Fanout, Loop}) plus a handful of Expr ops (path / lit / eq / mul /
    // call_extern), including every field-renamed wire tag (Step.ref/in,
    // Branch.then/else, CallExtern.ref).
    let full_json = json!({
        "kind": "try",
        "body": {
            "kind": "seq",
            "children": [
                {"kind": "step", "ref": "r1", "in": {"op": "path", "at": "$.in"}, "out": {"op": "path", "at": "$.out"}},
                {"kind": "branch",
                 "cond": {"op": "eq", "lhs": {"op": "lit", "value": 1}, "rhs": {"op": "lit", "value": 1}},
                 "then": {"kind": "assign", "at": {"op": "path", "at": "$.t"}, "value": {"op": "lit", "value": "y"}},
                 "else": {"kind": "assign", "at": {"op": "path", "at": "$.t"}, "value": {"op": "lit", "value": "n"}}},
                {"kind": "fanout",
                 "items": {"op": "lit", "value": [1, 2]},
                 "bind": {"op": "path", "at": "$.item"},
                 "body": {"kind": "assign", "at": {"op": "path", "at": "$.doubled"},
                          "value": {"op": "mul", "lhs": {"op": "path", "at": "$.item"}, "rhs": {"op": "lit", "value": 2}}},
                 "join": "all",
                 "out": {"op": "path", "at": "$.results"}},
                {"kind": "loop",
                 "counter": {"op": "path", "at": "$.c"},
                 "cond": {"op": "call_extern", "ref": "is_done", "args": [{"op": "path", "at": "$.x"}]},
                 "body": {"kind": "assign", "at": {"op": "path", "at": "$.x"}, "value": {"op": "lit", "value": 1}},
                 "max": 1}
            ]
        },
        "catch": {"kind": "assign", "at": {"op": "path", "at": "$.caught"}, "value": {"op": "lit", "value": true}},
        "err_at": {"op": "path", "at": "$.err"}
    });

    let node: Node = serde_json::from_value(full_json.clone()).unwrap();
    let reserialized = serde_json::to_value(&node).unwrap();
    let node2: Node = serde_json::from_value(reserialized.clone()).unwrap();
    let reserialized2 = serde_json::to_value(&node2).unwrap();

    // serialize(deserialize(serialize(x))) == serialize(x)
    assert_eq!(reserialized, reserialized2);
    // Node/Expr derive PartialEq — direct structural equality holds too.
    assert_eq!(node, node2);

    // Step: ref_ -> "ref", in_ -> "in" survive the round-trip.
    let step = &reserialized["body"]["children"][0];
    assert_eq!(step["ref"], json!("r1"));
    assert!(step.get("ref_").is_none());
    assert_eq!(step["in"]["op"], json!("path"));
    assert!(step.get("in_").is_none());

    // Branch: then_ -> "then", else_ -> "else" survive the round-trip.
    let branch = &reserialized["body"]["children"][1];
    assert_eq!(branch["then"]["kind"], json!("assign"));
    assert!(branch.get("then_").is_none());
    assert_eq!(branch["else"]["kind"], json!("assign"));
    assert!(branch.get("else_").is_none());

    // CallExtern (nested in Loop.cond): ref_ -> "ref" survives too.
    let loop_cond = &reserialized["body"]["children"][3]["cond"];
    assert_eq!(loop_cond["ref"], json!("is_done"));
    assert!(loop_cond.get("ref_").is_none());
}
