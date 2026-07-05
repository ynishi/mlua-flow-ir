//! Coverage for Expr ops added in stage 1 (comparison + boolean + existence)
//! and stage 2 (arithmetic + len + in), plus canonical-parity ops (mod /
//! call_extern) and canonical wire-format checks (gte / lte / args / arg).

use flow_ir_core::{eval_expr, eval_expr_with_externs, EvalError, Expr, ExternMap};
use serde_json::json;

fn lit(v: serde_json::Value) -> Box<Expr> {
    Box::new(Expr::Lit { value: v })
}

fn path(at: &str) -> Box<Expr> {
    Box::new(Expr::Path { at: at.into() })
}

// ──────────────────────────────────────────────────────────────────────────
// Stage 1: comparison
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn ne_op() {
    let ctx = json!({});
    assert_eq!(
        eval_expr(
            &Expr::Ne {
                lhs: lit(json!(1)),
                rhs: lit(json!(2))
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::Ne {
                lhs: lit(json!(1)),
                rhs: lit(json!(1))
            },
            &ctx
        )
        .unwrap(),
        json!(false)
    );
}

#[test]
fn lt_lte_gt_gte_ops() {
    let ctx = json!({});
    let e = Expr::Lt {
        lhs: lit(json!(1)),
        rhs: lit(json!(2)),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
    let e = Expr::Lte {
        lhs: lit(json!(2)),
        rhs: lit(json!(2)),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
    let e = Expr::Gt {
        lhs: lit(json!(3)),
        rhs: lit(json!(2)),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
    let e = Expr::Gte {
        lhs: lit(json!(2)),
        rhs: lit(json!(3)),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(false));
}

#[test]
fn cmp_strings_lexicographic() {
    // canonical Lua `<` compares strings; mirror it
    let ctx = json!({});
    let e = Expr::Lt {
        lhs: lit(json!("apple")),
        rhs: lit(json!("banana")),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
    let e = Expr::Gte {
        lhs: lit(json!("b")),
        rhs: lit(json!("b")),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
}

#[test]
fn cmp_rejects_mixed_types() {
    let ctx = json!({});
    // string vs number (canonical Lua raises on mixed compare too)
    let e = Expr::Lt {
        lhs: lit(json!("a")),
        rhs: lit(json!(2)),
    };
    assert!(eval_expr(&e, &ctx).is_err());
    // bool operands are not comparable
    let e = Expr::Gt {
        lhs: lit(json!(true)),
        rhs: lit(json!(false)),
    };
    assert!(eval_expr(&e, &ctx).is_err());
}

// ──────────────────────────────────────────────────────────────────────────
// Stage 1: boolean
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn not_op() {
    let ctx = json!({});
    assert_eq!(
        eval_expr(
            &Expr::Not {
                arg: lit(json!(false))
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(&Expr::Not { arg: lit(json!(0)) }, &ctx).unwrap(),
        // 0 is truthy in is_truthy semantics (only null/false are falsy)
        json!(false)
    );
    assert_eq!(
        eval_expr(
            &Expr::Not {
                arg: lit(json!(null))
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
}

#[test]
fn and_or_ops() {
    let ctx = json!({});
    let true_e = Expr::Lit { value: json!(true) };
    let false_e = Expr::Lit {
        value: json!(false),
    };

    assert_eq!(
        eval_expr(
            &Expr::And {
                args: vec![true_e.clone(), true_e.clone()]
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::And {
                args: vec![true_e.clone(), false_e.clone()]
            },
            &ctx
        )
        .unwrap(),
        json!(false)
    );
    // empty And = true (identity)
    assert_eq!(
        eval_expr(&Expr::And { args: vec![] }, &ctx).unwrap(),
        json!(true)
    );

    assert_eq!(
        eval_expr(
            &Expr::Or {
                args: vec![false_e.clone(), true_e.clone()]
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::Or {
                args: vec![false_e.clone(), false_e.clone()]
            },
            &ctx
        )
        .unwrap(),
        json!(false)
    );
    // empty Or = false (identity)
    assert_eq!(
        eval_expr(&Expr::Or { args: vec![] }, &ctx).unwrap(),
        json!(false)
    );
}

// ──────────────────────────────────────────────────────────────────────────
// Stage 1: existence (canonical form: arg is an Expr, truthy iff non-nil)
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn exists_op() {
    let ctx = json!({ "a": { "b": 1 }, "n": null });
    assert_eq!(
        eval_expr(&Expr::Exists { arg: path("$.a.b") }, &ctx).unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::Exists {
                arg: path("$.a.missing")
            },
            &ctx
        )
        .unwrap(),
        json!(false)
    );
    // canonical `arg ~= nil`: JSON null maps to Lua nil → does NOT exist
    assert_eq!(
        eval_expr(&Expr::Exists { arg: path("$.n") }, &ctx).unwrap(),
        json!(false)
    );
    // non-path arg: any non-null value exists
    assert_eq!(
        eval_expr(
            &Expr::Exists {
                arg: lit(json!(false))
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
}

// ──────────────────────────────────────────────────────────────────────────
// Stage 2: arithmetic
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn add_sub_mul_div_ops() {
    let ctx = json!({});
    assert_eq!(
        eval_expr(
            &Expr::Add {
                lhs: lit(json!(2)),
                rhs: lit(json!(3))
            },
            &ctx
        )
        .unwrap(),
        json!(5.0)
    );
    assert_eq!(
        eval_expr(
            &Expr::Sub {
                lhs: lit(json!(5)),
                rhs: lit(json!(3))
            },
            &ctx
        )
        .unwrap(),
        json!(2.0)
    );
    assert_eq!(
        eval_expr(
            &Expr::Mul {
                lhs: lit(json!(4)),
                rhs: lit(json!(3))
            },
            &ctx
        )
        .unwrap(),
        json!(12.0)
    );
    assert_eq!(
        eval_expr(
            &Expr::Div {
                lhs: lit(json!(10)),
                rhs: lit(json!(4))
            },
            &ctx
        )
        .unwrap(),
        json!(2.5)
    );
}

#[test]
fn mod_op_lua_semantics() {
    let ctx = json!({});
    assert_eq!(
        eval_expr(
            &Expr::Mod {
                lhs: lit(json!(7)),
                rhs: lit(json!(3))
            },
            &ctx
        )
        .unwrap(),
        json!(1.0)
    );
    // Lua `%`: result takes the sign of rhs → -7 % 3 == 2 (not -1)
    assert_eq!(
        eval_expr(
            &Expr::Mod {
                lhs: lit(json!(-7)),
                rhs: lit(json!(3))
            },
            &ctx
        )
        .unwrap(),
        json!(2.0)
    );
    // mod by zero rejected (canonical parity)
    assert!(eval_expr(
        &Expr::Mod {
            lhs: lit(json!(1)),
            rhs: lit(json!(0))
        },
        &ctx
    )
    .is_err());
}

#[test]
fn div_by_zero_errors() {
    let ctx = json!({});
    let e = Expr::Div {
        lhs: lit(json!(1)),
        rhs: lit(json!(0)),
    };
    assert!(eval_expr(&e, &ctx).is_err());
}

#[test]
fn arith_via_path() {
    let ctx = json!({ "x": 10, "y": 7 });
    let e = Expr::Sub {
        lhs: path("$.x"),
        rhs: path("$.y"),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(3.0));
}

// ──────────────────────────────────────────────────────────────────────────
// Stage 2: aggregate
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn len_op() {
    let ctx = json!({ "arr": [1, 2, 3], "s": "hello", "o": {"a": 1, "b": 2} });
    assert_eq!(
        eval_expr(&Expr::Len { arg: path("$.arr") }, &ctx).unwrap(),
        json!(3)
    );
    assert_eq!(
        eval_expr(&Expr::Len { arg: path("$.s") }, &ctx).unwrap(),
        json!(5)
    );
    assert_eq!(
        eval_expr(&Expr::Len { arg: path("$.o") }, &ctx).unwrap(),
        json!(2)
    );
    // Number / bool / null are not len-able
    assert!(eval_expr(
        &Expr::Len {
            arg: lit(json!(42))
        },
        &ctx
    )
    .is_err());
}

#[test]
fn in_op() {
    let ctx = json!({ "items": ["a", "b", "c"] });
    assert_eq!(
        eval_expr(
            &Expr::In {
                needle: lit(json!("b")),
                haystack: path("$.items")
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::In {
                needle: lit(json!("z")),
                haystack: path("$.items")
            },
            &ctx
        )
        .unwrap(),
        json!(false)
    );
    // Non-array haystack rejected
    assert!(eval_expr(
        &Expr::In {
            needle: lit(json!(1)),
            haystack: lit(json!("scalar"))
        },
        &ctx
    )
    .is_err());
}

// ──────────────────────────────────────────────────────────────────────────
// call_extern — value-shape Hatch via Externs registry
// ──────────────────────────────────────────────────────────────────────────

fn arg_f64(args: &[serde_json::Value], i: usize, ref_: &str) -> Result<f64, EvalError> {
    args.get(i)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| EvalError::ExternError {
            ref_: ref_.into(),
            msg: format!("arg[{i}] must be a number"),
        })
}

#[test]
fn call_extern_resolves_registered_fn() {
    let mut externs = ExternMap::new();
    externs.register("math.sqrt", |args: &[serde_json::Value]| {
        Ok(json!(arg_f64(args, 0, "math.sqrt")?.sqrt()))
    });
    externs.register("math.ln", |args: &[serde_json::Value]| {
        Ok(json!(arg_f64(args, 0, "math.ln")?.ln()))
    });

    let ctx = json!({ "n": 9.0 });
    // UCB1-shaped nesting: sqrt(ln(e^4) * n) = sqrt(4 * 9) = 6
    let e = Expr::CallExtern {
        ref_: "math.sqrt".into(),
        args: vec![Expr::Mul {
            lhs: Box::new(Expr::CallExtern {
                ref_: "math.ln".into(),
                args: vec![Expr::Lit {
                    value: json!(std::f64::consts::E.powi(4)),
                }],
            }),
            rhs: Box::new(Expr::Path { at: "$.n".into() }),
        }],
    };
    let out = eval_expr_with_externs(&e, &ctx, &externs).unwrap();
    let got = out.as_f64().unwrap();
    assert!((got - 6.0).abs() < 1e-9, "got {got}");
}

#[test]
fn call_extern_unregistered_ref_errors() {
    let externs = ExternMap::new();
    let e = Expr::CallExtern {
        ref_: "nope".into(),
        args: vec![],
    };
    let err = eval_expr_with_externs(&e, &json!({}), &externs).unwrap_err();
    assert!(matches!(err, EvalError::ExternError { .. }), "{err:?}");
}

#[test]
fn call_extern_without_registry_errors() {
    // externs-less compat wrapper (`eval_expr`) must raise, mirroring
    // canonical "requires opts.externs" error
    let e = Expr::CallExtern {
        ref_: "math.sqrt".into(),
        args: vec![],
    };
    let err = eval_expr(&e, &json!({})).unwrap_err();
    assert!(matches!(err, EvalError::ExternError { .. }), "{err:?}");
}

// ──────────────────────────────────────────────────────────────────────────
// Schema: canonical wire format (op tags + field names)
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn parse_new_ops_from_json() {
    let json_src = serde_json::json!({
        "op": "and",
        "args": [
            { "op": "lt", "lhs": { "op": "path", "at": "$.x" }, "rhs": { "op": "lit", "value": 10 } },
            { "op": "exists", "arg": { "op": "path", "at": "$.flag" } }
        ]
    });
    let e: Expr = serde_json::from_value(json_src).unwrap();
    let ctx = json!({ "x": 5, "flag": true });
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
}

#[test]
fn parse_canonical_gte_lte_mod_call_extern() {
    // canonical flow-ir-lua wire tags: gte / lte (NOT ge / le)
    let e: Expr = serde_json::from_value(json!({
        "op": "gte",
        "lhs": { "op": "lit", "value": 3 },
        "rhs": { "op": "lit", "value": 3 },
    }))
    .unwrap();
    assert_eq!(eval_expr(&e, &json!({})).unwrap(), json!(true));

    let e: Expr = serde_json::from_value(json!({
        "op": "lte",
        "lhs": { "op": "lit", "value": 4 },
        "rhs": { "op": "lit", "value": 3 },
    }))
    .unwrap();
    assert_eq!(eval_expr(&e, &json!({})).unwrap(), json!(false));

    // legacy ge / le tags must NOT parse (canonical is SoT)
    assert!(serde_json::from_value::<Expr>(json!({
        "op": "ge",
        "lhs": { "op": "lit", "value": 1 },
        "rhs": { "op": "lit", "value": 1 },
    }))
    .is_err());

    let e: Expr = serde_json::from_value(json!({
        "op": "mod",
        "lhs": { "op": "lit", "value": 7 },
        "rhs": { "op": "lit", "value": 3 },
    }))
    .unwrap();
    assert_eq!(eval_expr(&e, &json!({})).unwrap(), json!(1.0));

    let e: Expr = serde_json::from_value(json!({
        "op": "call_extern",
        "ref": "id",
        "args": [{ "op": "lit", "value": 42 }],
    }))
    .unwrap();
    let mut externs = ExternMap::new();
    externs.register("id", |args: &[serde_json::Value]| Ok(args[0].clone()));
    assert_eq!(
        eval_expr_with_externs(&e, &json!({}), &externs).unwrap(),
        json!(42)
    );
}
