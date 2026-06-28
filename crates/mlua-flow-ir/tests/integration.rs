use mlua_flow_ir::{eval, eval_expr, Dispatcher, EvalError, Expr, Node};
use serde_json::{json, Value};

// ──────────────────────────────────────────────────────────────────────────
// Fixture dispatcher (covers the 2 refs used in tests below)
// ──────────────────────────────────────────────────────────────────────────

struct FixtureDispatcher;

impl Dispatcher for FixtureDispatcher {
    fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError> {
        match ref_ {
            "uppercase" => match input {
                Value::String(s) => Ok(Value::String(s.to_uppercase())),
                other => Err(EvalError::DispatcherError {
                    ref_: ref_.into(),
                    msg: format!("expect string, got {}", other),
                }),
            },
            "count_one" => Ok(Value::Number(1.into())),
            _ => Err(EvalError::DispatcherError {
                ref_: ref_.into(),
                msg: "unknown ref".into(),
            }),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Expr: 3 ops
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn expr_lit_returns_literal() {
    let e = Expr::Lit { value: json!(42) };
    assert_eq!(eval_expr(&e, &json!({})).unwrap(), json!(42));
}

#[test]
fn expr_path_root() {
    let e = Expr::Path { at: "$".into() };
    let ctx = json!({ "k": "v" });
    assert_eq!(eval_expr(&e, &ctx).unwrap(), ctx);
}

#[test]
fn expr_path_reads_nested() {
    let e = Expr::Path { at: "$.a.b".into() };
    assert_eq!(
        eval_expr(&e, &json!({ "a": { "b": "hello" } })).unwrap(),
        json!("hello")
    );
}

#[test]
fn expr_path_missing_returns_error() {
    let e = Expr::Path {
        at: "$.missing".into(),
    };
    assert!(matches!(
        eval_expr(&e, &json!({})),
        Err(EvalError::PathNotFound(_))
    ));
}

#[test]
fn expr_path_invalid_prefix() {
    let e = Expr::Path {
        at: "no.prefix".into(),
    };
    assert!(matches!(
        eval_expr(&e, &json!({})),
        Err(EvalError::InvalidPath(_))
    ));
}

#[test]
fn expr_eq_returns_bool() {
    let e = Expr::Eq {
        lhs: Box::new(Expr::Path { at: "$.x".into() }),
        rhs: Box::new(Expr::Lit { value: json!("ok") }),
    };
    assert_eq!(eval_expr(&e, &json!({ "x": "ok" })).unwrap(), json!(true));
    assert_eq!(eval_expr(&e, &json!({ "x": "ng" })).unwrap(), json!(false));
}

// ──────────────────────────────────────────────────────────────────────────
// Node: 3 kinds
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn node_step_writes_output_to_ctx() {
    let n = Node::Step {
        ref_: "uppercase".into(),
        in_: Expr::Path {
            at: "$.input".into(),
        },
        out: Expr::Path {
            at: "$.output".into(),
        },
    };
    let result = eval(&n, json!({ "input": "hello" }), &FixtureDispatcher).unwrap();
    assert_eq!(result, json!({ "input": "hello", "output": "HELLO" }));
}

#[test]
fn node_step_writes_nested_output() {
    let n = Node::Step {
        ref_: "count_one".into(),
        in_: Expr::Lit { value: json!(null) },
        out: Expr::Path {
            at: "$.deep.nested.count".into(),
        },
    };
    let result = eval(&n, json!({}), &FixtureDispatcher).unwrap();
    assert_eq!(result, json!({ "deep": { "nested": { "count": 1 } } }));
}

#[test]
fn node_step_dispatcher_error_propagates() {
    let n = Node::Step {
        ref_: "unknown_ref".into(),
        in_: Expr::Lit { value: json!(null) },
        out: Expr::Path { at: "$.x".into() },
    };
    assert!(matches!(
        eval(&n, json!({}), &FixtureDispatcher),
        Err(EvalError::DispatcherError { .. })
    ));
}

#[test]
fn node_seq_chains_steps() {
    let n = Node::Seq {
        children: vec![
            Node::Step {
                ref_: "uppercase".into(),
                in_: Expr::Path {
                    at: "$.input".into(),
                },
                out: Expr::Path { at: "$.up".into() },
            },
            Node::Step {
                ref_: "count_one".into(),
                in_: Expr::Lit { value: json!(null) },
                out: Expr::Path {
                    at: "$.count".into(),
                },
            },
        ],
    };
    let result = eval(&n, json!({ "input": "world" }), &FixtureDispatcher).unwrap();
    assert_eq!(
        result,
        json!({ "input": "world", "up": "WORLD", "count": 1 })
    );
}

#[test]
fn node_branch_then_path() {
    let n = make_branch();
    let then_result = eval(
        &n,
        json!({ "flag": true, "input": "hi" }),
        &FixtureDispatcher,
    )
    .unwrap();
    assert_eq!(then_result["result"], json!("HI"));
}

#[test]
fn node_branch_else_path() {
    let n = make_branch();
    let else_result = eval(&n, json!({ "flag": false }), &FixtureDispatcher).unwrap();
    assert_eq!(else_result["result"], json!(1));
}

#[test]
fn node_branch_non_bool_cond_errors() {
    let n = Node::Branch {
        cond: Expr::Lit {
            value: json!("not a bool"),
        },
        then_: Box::new(Node::Seq { children: vec![] }),
        else_: Box::new(Node::Seq { children: vec![] }),
    };
    assert!(matches!(
        eval(&n, json!({}), &FixtureDispatcher),
        Err(EvalError::NonBoolCond(_))
    ));
}

fn make_branch() -> Node {
    Node::Branch {
        cond: Expr::Eq {
            lhs: Box::new(Expr::Path {
                at: "$.flag".into(),
            }),
            rhs: Box::new(Expr::Lit { value: json!(true) }),
        },
        then_: Box::new(Node::Step {
            ref_: "uppercase".into(),
            in_: Expr::Path {
                at: "$.input".into(),
            },
            out: Expr::Path {
                at: "$.result".into(),
            },
        }),
        else_: Box::new(Node::Step {
            ref_: "count_one".into(),
            in_: Expr::Lit { value: json!(null) },
            out: Expr::Path {
                at: "$.result".into(),
            },
        }),
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Serde: discriminated + deny_unknown_fields
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn serde_roundtrip_step() {
    let n: Node = serde_json::from_value(json!({
        "kind": "step",
        "ref": "test_ref",
        "in": { "op": "lit", "value": "x" },
        "out": { "op": "path", "at": "$.r" },
    }))
    .unwrap();
    let serialized = serde_json::to_value(&n).unwrap();
    assert_eq!(serialized["kind"], "step");
    assert_eq!(serialized["ref"], "test_ref");
}

#[test]
fn serde_roundtrip_seq_and_branch() {
    let v = json!({
        "kind": "seq",
        "children": [
            {
                "kind": "branch",
                "cond": { "op": "lit", "value": true },
                "then": { "kind": "step", "ref": "a", "in": { "op": "lit", "value": null }, "out": { "op": "path", "at": "$.a" } },
                "else": { "kind": "step", "ref": "b", "in": { "op": "lit", "value": null }, "out": { "op": "path", "at": "$.b" } },
            }
        ]
    });
    let n: Node = serde_json::from_value(v.clone()).unwrap();
    let back = serde_json::to_value(&n).unwrap();
    let n2: Node = serde_json::from_value(back).unwrap();
    assert_eq!(n, n2);
}

#[test]
fn serde_rejects_unknown_field_in_node() {
    let r: Result<Node, _> = serde_json::from_value(json!({
        "kind": "step",
        "ref": "x",
        "in": { "op": "lit", "value": null },
        "out": { "op": "path", "at": "$.r" },
        "extra_field": "not allowed",
    }));
    assert!(r.is_err(), "deny_unknown_fields should reject extras");
}

#[test]
fn serde_rejects_unknown_op_in_expr() {
    let r: Result<Expr, _> = serde_json::from_value(json!({ "op": "not_an_op", "value": 1 }));
    assert!(r.is_err(), "unknown op should be rejected");
}

// ──────────────────────────────────────────────────────────────────────────
// Closure dispatcher (blanket impl)
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn closure_dispatcher_works() {
    let dispatcher =
        |_r: &str, _input: Value| -> Result<Value, EvalError> { Ok(json!("closure-result")) };
    let n = Node::Step {
        ref_: "anything".into(),
        in_: Expr::Lit { value: json!(null) },
        out: Expr::Path { at: "$.r".into() },
    };
    let result = eval(&n, json!({}), &dispatcher).unwrap();
    assert_eq!(result, json!({ "r": "closure-result" }));
}
