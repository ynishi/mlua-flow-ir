use async_trait::async_trait;
use mlua_flow_ir::{eval_async, AsyncDispatcher, EvalError, Expr, Node};
use serde_json::{json, Value};

// ──────────────────────────────────────────────────────────────────────────
// Fixture async dispatcher
// ──────────────────────────────────────────────────────────────────────────

struct FixtureAsyncDispatcher;

#[async_trait]
impl AsyncDispatcher for FixtureAsyncDispatcher {
    async fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError> {
        match ref_ {
            "uppercase" => match input {
                Value::String(s) => Ok(Value::String(s.to_uppercase())),
                other => Err(EvalError::DispatcherError {
                    ref_: ref_.into(),
                    msg: format!("expect string, got {}", other),
                }),
            },
            "count_one" => Ok(Value::Number(1.into())),
            "delay_echo" => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                Ok(input)
            }
            _ => Err(EvalError::DispatcherError {
                ref_: ref_.into(),
                msg: "unknown ref".into(),
            }),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Node: 3 kinds (async)
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn eval_async_node_step() {
    let n = Node::Step {
        ref_: "uppercase".into(),
        in_: Expr::Path {
            at: "$.input".into(),
        },
        out: Expr::Path {
            at: "$.output".into(),
        },
    };
    let r = eval_async(&n, json!({ "input": "hi" }), &FixtureAsyncDispatcher)
        .await
        .unwrap();
    assert_eq!(r, json!({ "input": "hi", "output": "HI" }));
}

#[tokio::test]
async fn eval_async_node_seq() {
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
    let r = eval_async(&n, json!({ "input": "w" }), &FixtureAsyncDispatcher)
        .await
        .unwrap();
    assert_eq!(r, json!({ "input": "w", "up": "W", "count": 1 }));
}

#[tokio::test]
async fn eval_async_node_branch_then() {
    let n = make_branch();
    let r = eval_async(
        &n,
        json!({ "flag": true, "input": "y" }),
        &FixtureAsyncDispatcher,
    )
    .await
    .unwrap();
    assert_eq!(r["result"], json!("Y"));
}

#[tokio::test]
async fn eval_async_node_branch_else() {
    let n = make_branch();
    let r = eval_async(&n, json!({ "flag": false }), &FixtureAsyncDispatcher)
        .await
        .unwrap();
    assert_eq!(r["result"], json!(1));
}

#[tokio::test]
async fn eval_async_branch_non_bool_cond_errors() {
    let n = Node::Branch {
        cond: Expr::Lit { value: json!("not bool") },
        then_: Box::new(Node::Seq { children: vec![] }),
        else_: Box::new(Node::Seq { children: vec![] }),
    };
    let r = eval_async(&n, json!({}), &FixtureAsyncDispatcher).await;
    assert!(matches!(r, Err(EvalError::NonBoolCond(_))));
}

#[tokio::test]
async fn eval_async_dispatcher_error_propagates() {
    let n = Node::Step {
        ref_: "unknown".into(),
        in_: Expr::Lit { value: json!(null) },
        out: Expr::Path { at: "$.x".into() },
    };
    let r = eval_async(&n, json!({}), &FixtureAsyncDispatcher).await;
    assert!(matches!(r, Err(EvalError::DispatcherError { .. })));
}

// ──────────────────────────────────────────────────────────────────────────
// dyn safe (= async_trait の効果確認)
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn eval_async_accepts_dyn_dispatcher() {
    let n = Node::Step {
        ref_: "uppercase".into(),
        in_: Expr::Lit { value: json!("x") },
        out: Expr::Path { at: "$.out".into() },
    };
    let dispatcher: &dyn AsyncDispatcher = &FixtureAsyncDispatcher;
    let r = eval_async(&n, json!({}), dispatcher).await.unwrap();
    assert_eq!(r["out"], json!("X"));
}

// ──────────────────────────────────────────────────────────────────────────
// Nested deep blueprint (= recursive async eval)
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn eval_async_nested_seq_branch_seq() {
    let inner_seq = Node::Seq {
        children: vec![
            Node::Step {
                ref_: "uppercase".into(),
                in_: Expr::Path {
                    at: "$.input".into(),
                },
                out: Expr::Path { at: "$.a".into() },
            },
            Node::Step {
                ref_: "count_one".into(),
                in_: Expr::Lit { value: json!(null) },
                out: Expr::Path { at: "$.b".into() },
            },
        ],
    };
    let outer = Node::Seq {
        children: vec![Node::Branch {
            cond: Expr::Lit { value: json!(true) },
            then_: Box::new(inner_seq),
            else_: Box::new(Node::Step {
                ref_: "count_one".into(),
                in_: Expr::Lit { value: json!(null) },
                out: Expr::Path {
                    at: "$.skipped".into(),
                },
            }),
        }],
    };
    let r = eval_async(&outer, json!({ "input": "deep" }), &FixtureAsyncDispatcher)
        .await
        .unwrap();
    assert_eq!(r["a"], json!("DEEP"));
    assert_eq!(r["b"], json!(1));
    assert!(r.get("skipped").is_none());
}

// ──────────────────────────────────────────────────────────────────────────
// True async (= dispatch が .await を含む)
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn eval_async_with_actual_await_in_dispatch() {
    let n = Node::Seq {
        children: vec![
            Node::Step {
                ref_: "delay_echo".into(),
                in_: Expr::Path {
                    at: "$.input".into(),
                },
                out: Expr::Path { at: "$.r1".into() },
            },
            Node::Step {
                ref_: "delay_echo".into(),
                in_: Expr::Path {
                    at: "$.r1".into(),
                },
                out: Expr::Path { at: "$.r2".into() },
            },
        ],
    };
    let r = eval_async(&n, json!({ "input": "hello" }), &FixtureAsyncDispatcher)
        .await
        .unwrap();
    assert_eq!(r["r1"], json!("hello"));
    assert_eq!(r["r2"], json!("hello"));
}

// ──────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────

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
