//! E2E dynamic State injection tests.
//!
//! 目的: Flow が **実行中** (= `eval_async_with_storage` の Step が
//! `dispatch().await` で suspend している瞬間) に、 外部 tokio task が同じ
//! `Arc<dyn CtxStorage>` を経由して ctx を mutate し、 その変更が resume 後の
//! 後続 Step で観測されることを **mock なし** で証明する。
//!
//! 不正証明 (= "事前コンパイルした Assign を IR に仕込んだだけ" を排除する
//! ための制約):
//!
//! 1. Flow IR には `Node::Assign` を **一切含めない** (Step のみ)
//! 2. dispatcher は 1 経路で本物の `tokio::sync::oneshot` を用いて suspend
//! 3. 外部 task は **dispatcher が ready を signal した後** に ctx.write を
//!    実行 (= dispatch.await の suspend 期間中であることを barrier で保証)
//! 4. 観測軸は ctx の最終 snapshot の値 (= 後続 Step が dispatch 前に
//!    snapshot を取って Expr eval した結果が反映されているか)

use async_trait::async_trait;
use mlua_flow_ir::{
    eval_async_with_storage, AsyncDispatcher, CtxStorage, EvalError, Expr, MemoryCtx, Node,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// "wait_for_signal" / "read_injected" 2 ref を扱う dispatcher。
///
/// "wait_for_signal" は呼ばれた瞬間に `ready_tx` で外部に「suspend 入った」
/// と通知し、 続いて `release_rx` を await して外部 task からの「resume せよ」
/// を待つ。 → 本物の async suspend、 mock ではない。
struct ChannelDispatcher {
    ready_tx: Mutex<Option<oneshot::Sender<()>>>,
    release_rx: Mutex<Option<oneshot::Receiver<()>>>,
}

#[async_trait]
impl AsyncDispatcher for ChannelDispatcher {
    async fn dispatch(&self, ref_: &str, input: Value) -> Result<Value, EvalError> {
        match ref_ {
            "wait_for_signal" => {
                let ready = self
                    .ready_tx
                    .lock()
                    .unwrap()
                    .take()
                    .expect("wait_for_signal called twice");
                ready.send(()).expect("ready receiver dropped");
                let release = self
                    .release_rx
                    .lock()
                    .unwrap()
                    .take()
                    .expect("release receiver missing");
                release.await.expect("release sender dropped");
                Ok(json!({"signaled": true, "input": input}))
            }
            "echo" => Ok(input),
            other => Err(EvalError::DispatcherError {
                ref_: other.into(),
                msg: format!("unknown ref: {other}"),
            }),
        }
    }
}

/// 真の dynamic injection E2E。
///
/// Flow IR: Seq([
///   Step("wait_for_signal"), -- dispatch.await suspend → 外部書込み →
///                              resume + signal_received に書込
///   Step("echo"),            -- $.injected を input にして $.echoed に echo
/// ])
///
/// 外部 task: ready 受信 → ctx.write("$.injected", 42) → release 送信
///
/// 観測:
///   - $.signal_received = {"signaled": true, ...}
///   - $.echoed = 42  (= 外部 task が dispatch.await 中に書いた値が
///                      後続 Step の snapshot に乗っている = dynamic injection 成立)
#[tokio::test]
async fn external_task_injects_state_during_step_await() {
    let storage: Arc<dyn CtxStorage> = MemoryCtx::shared(json!({ "seed": "start" }));

    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let dispatcher = ChannelDispatcher {
        ready_tx: Mutex::new(Some(ready_tx)),
        release_rx: Mutex::new(Some(release_rx)),
    };

    // Flow IR: Step だけで構成、 Node::Assign は **一切含めない**
    let flow = Node::Seq {
        children: vec![
            Node::Step {
                ref_: "wait_for_signal".into(),
                in_: Expr::Path {
                    at: "$.seed".parse().unwrap(),
                },
                out: Expr::Path {
                    at: "$.signal_received".parse().unwrap(),
                },
            },
            Node::Step {
                ref_: "echo".into(),
                in_: Expr::Path {
                    at: "$.injected".parse().unwrap(),
                },
                out: Expr::Path {
                    at: "$.echoed".parse().unwrap(),
                },
            },
        ],
    };

    // 外部 mutator: dispatch.await 中に共有 ctx を直接 write
    let storage_for_external = storage.clone();
    let mutator = tokio::spawn(async move {
        // dispatcher が suspend に入ったのを確認してから書く
        ready_rx.await.expect("ready not received");
        storage_for_external
            .write("$.injected", json!(42))
            .expect("ctx.write failed");
        // 後続 Step が "echo" を呼ぶ前 (= snapshot を取る前) に release
        release_tx.send(()).expect("release send failed");
    });

    eval_async_with_storage(&flow, storage.clone(), &dispatcher)
        .await
        .expect("eval_async_with_storage failed");
    mutator.await.expect("mutator task panicked");

    let final_ctx = storage.snapshot();
    // Step 1 の出力 (dispatch 戻り値) が書き込まれていること
    assert_eq!(
        final_ctx.pointer("/signal_received/signaled"),
        Some(&json!(true)),
        "Step 1 dispatch result not written: {final_ctx:#?}"
    );
    // 真の証明: 外部 task が dispatch.await 中に書いた値が
    // Step 2 の input (snapshot 経由) として読まれ、 echo 経由で out に届いている
    assert_eq!(
        final_ctx.pointer("/echoed"),
        Some(&json!(42)),
        "External write during suspend was NOT observed by next Step: {final_ctx:#?}"
    );
    // Flow IR 内に Assign は無いので、 $.injected が ctx に存在する事実自体
    // が「IR 外からの動的注入」 の証拠
    assert_eq!(final_ctx.pointer("/injected"), Some(&json!(42)));
}

/// 二段目の証明: 外部 task が `eval_expr` を **runtime に動的構築** して
/// CtxStorage 経由で評価結果を書く path も成立すること (= 「Eval を投げる」
/// の literal 解釈)。
///
/// Flow IR は Step("wait_for_signal") 1 個だけ。 外部 task は新規 Expr を
/// 構築 → snapshot で eval_expr → ctx.write の経路で State を更新する。
#[tokio::test]
async fn external_task_constructs_expr_at_runtime_and_writes() {
    use mlua_flow_ir::eval_expr;

    let storage: Arc<dyn CtxStorage> = MemoryCtx::shared(json!({ "base": 100, "multiplier": 3 }));

    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let dispatcher = ChannelDispatcher {
        ready_tx: Mutex::new(Some(ready_tx)),
        release_rx: Mutex::new(Some(release_rx)),
    };

    let flow = Node::Step {
        ref_: "wait_for_signal".into(),
        in_: Expr::Path {
            at: "$.base".parse().unwrap(),
        },
        out: Expr::Path {
            at: "$.done".parse().unwrap(),
        },
    };

    let storage_for_external = storage.clone();
    let mutator = tokio::spawn(async move {
        ready_rx.await.expect("ready not received");

        // **runtime に Expr tree を構築** (= IR には書かれていない、 動的注入)
        let dynamic_expr = Expr::Path {
            at: "$.base".parse().unwrap(),
        };
        let snap = storage_for_external.snapshot();
        let resolved = eval_expr(&dynamic_expr, &snap).expect("runtime eval_expr failed");
        // 評価結果を別 path に書く (Assign Node 経由ではない、 直 write)
        storage_for_external
            .write("$.runtime_injected", resolved)
            .expect("ctx.write failed");

        release_tx.send(()).expect("release send failed");
    });

    eval_async_with_storage(&flow, storage.clone(), &dispatcher)
        .await
        .expect("eval failed");
    mutator.await.expect("mutator panicked");

    let final_ctx = storage.snapshot();
    assert_eq!(
        final_ctx.pointer("/runtime_injected"),
        Some(&json!(100)),
        "runtime-constructed Expr eval result not written: {final_ctx:#?}"
    );
}

/// Assign Node primitive 単体の smoke (E2E ではない、 IR primitive 確認用)。
/// 既存 IR と Assign の co-existence、 Seq での Step ↔ Assign 混在を検証。
#[tokio::test]
async fn assign_node_writes_value_inline() {
    struct EchoDispatcher;
    #[async_trait]
    impl AsyncDispatcher for EchoDispatcher {
        async fn dispatch(&self, _ref_: &str, input: Value) -> Result<Value, EvalError> {
            Ok(input)
        }
    }

    let storage = MemoryCtx::shared(json!({ "x": 1 }));
    let flow = Node::Seq {
        children: vec![
            Node::Assign {
                at: Expr::Path {
                    at: "$.y".parse().unwrap(),
                },
                value: Expr::Lit { value: json!(42) },
            },
            Node::Step {
                ref_: "echo".into(),
                in_: Expr::Path {
                    at: "$.y".parse().unwrap(),
                },
                out: Expr::Path {
                    at: "$.z".parse().unwrap(),
                },
            },
        ],
    };

    eval_async_with_storage(&flow, storage.clone(), &EchoDispatcher)
        .await
        .expect("eval failed");

    let final_ctx = storage.snapshot();
    assert_eq!(final_ctx.pointer("/y"), Some(&json!(42)));
    assert_eq!(final_ctx.pointer("/z"), Some(&json!(42)));
}
