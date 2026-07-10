use async_trait::async_trait;
use mlua_flow_ir::{
    eval_async, eval_async_externs, AsyncDispatcher, EvalError, Expr, ExternMap, Node,
};
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
            at: "$.input".parse().unwrap(),
        },
        out: Expr::Path {
            at: "$.output".parse().unwrap(),
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
                    at: "$.input".parse().unwrap(),
                },
                out: Expr::Path {
                    at: "$.up".parse().unwrap(),
                },
            },
            Node::Step {
                ref_: "count_one".into(),
                in_: Expr::Lit { value: json!(null) },
                out: Expr::Path {
                    at: "$.count".parse().unwrap(),
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
        cond: Expr::Lit {
            value: json!("not bool"),
        },
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
        out: Expr::Path {
            at: "$.x".parse().unwrap(),
        },
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
        out: Expr::Path {
            at: "$.out".parse().unwrap(),
        },
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
                    at: "$.input".parse().unwrap(),
                },
                out: Expr::Path {
                    at: "$.a".parse().unwrap(),
                },
            },
            Node::Step {
                ref_: "count_one".into(),
                in_: Expr::Lit { value: json!(null) },
                out: Expr::Path {
                    at: "$.b".parse().unwrap(),
                },
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
                    at: "$.skipped".parse().unwrap(),
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
                    at: "$.input".parse().unwrap(),
                },
                out: Expr::Path {
                    at: "$.r1".parse().unwrap(),
                },
            },
            Node::Step {
                ref_: "delay_echo".into(),
                in_: Expr::Path {
                    at: "$.r1".parse().unwrap(),
                },
                out: Expr::Path {
                    at: "$.r2".parse().unwrap(),
                },
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
// call_extern via async path (externs must keep the future Send)
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn eval_async_call_extern_with_extern_map() {
    let mut externs = ExternMap::new();
    externs.register("math.sqrt", |args: &[Value]| {
        let x = args[0].as_f64().ok_or_else(|| EvalError::ExternError {
            ref_: "math.sqrt".into(),
            msg: "expected number".into(),
        })?;
        Ok(json!(x.sqrt()))
    });

    let n = Node::Assign {
        at: Expr::Path {
            at: "$.root".parse().unwrap(),
        },
        value: Expr::CallExtern {
            ref_: "math.sqrt".into(),
            args: vec![Expr::Path {
                at: "$.n".parse().unwrap(),
            }],
        },
    };
    // spawn 経由で走らせて future が Send であることも同時に検証
    let handle = tokio::spawn(async move {
        eval_async_externs(&n, json!({ "n": 16.0 }), &FixtureAsyncDispatcher, &externs).await
    });
    let r = handle.await.unwrap().unwrap();
    assert_eq!(r["root"], json!(4.0));
}

#[tokio::test]
async fn eval_async_call_extern_without_externs_errors() {
    let n = Node::Assign {
        at: Expr::Path {
            at: "$.x".parse().unwrap(),
        },
        value: Expr::CallExtern {
            ref_: "f".into(),
            args: vec![],
        },
    };
    let err = eval_async(&n, json!({}), &FixtureAsyncDispatcher)
        .await
        .unwrap_err();
    assert!(matches!(err, EvalError::ExternError { .. }), "{err:?}");
}

// ──────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────

fn make_branch() -> Node {
    Node::Branch {
        cond: Expr::Eq {
            lhs: Box::new(Expr::Path {
                at: "$.flag".parse().unwrap(),
            }),
            rhs: Box::new(Expr::Lit { value: json!(true) }),
        },
        then_: Box::new(Node::Step {
            ref_: "uppercase".into(),
            in_: Expr::Path {
                at: "$.input".parse().unwrap(),
            },
            out: Expr::Path {
                at: "$.result".parse().unwrap(),
            },
        }),
        else_: Box::new(Node::Step {
            ref_: "count_one".into(),
            in_: Expr::Lit { value: json!(null) },
            out: Expr::Path {
                at: "$.result".parse().unwrap(),
            },
        }),
    }
}
