//! Coverage for Expr ops added in stage 1 (comparison + boolean + existence)
//! and stage 2 (arithmetic + len + in). Each op gets at least one truthy
//! and one falsy / error case.

use flow_ir_core::{eval_expr, Expr};
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
fn lt_le_gt_ge_ops() {
    let ctx = json!({});
    let e = Expr::Lt {
        lhs: lit(json!(1)),
        rhs: lit(json!(2)),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
    let e = Expr::Le {
        lhs: lit(json!(2)),
        rhs: lit(json!(2)),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
    let e = Expr::Gt {
        lhs: lit(json!(3)),
        rhs: lit(json!(2)),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
    let e = Expr::Ge {
        lhs: lit(json!(2)),
        rhs: lit(json!(3)),
    };
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(false));
}

#[test]
fn cmp_rejects_non_number() {
    let ctx = json!({});
    let e = Expr::Lt {
        lhs: lit(json!("a")),
        rhs: lit(json!(2)),
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
                operand: lit(json!(false))
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::Not {
                operand: lit(json!(0))
            },
            &ctx
        )
        .unwrap(),
        // 0 is truthy in is_truthy semantics (only null/false are falsy)
        json!(false)
    );
    assert_eq!(
        eval_expr(
            &Expr::Not {
                operand: lit(json!(null))
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
                operands: vec![true_e.clone(), true_e.clone()]
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::And {
                operands: vec![true_e.clone(), false_e.clone()]
            },
            &ctx
        )
        .unwrap(),
        json!(false)
    );
    // empty And = true (identity)
    assert_eq!(
        eval_expr(&Expr::And { operands: vec![] }, &ctx).unwrap(),
        json!(true)
    );

    assert_eq!(
        eval_expr(
            &Expr::Or {
                operands: vec![false_e.clone(), true_e.clone()]
            },
            &ctx
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::Or {
                operands: vec![false_e.clone(), false_e.clone()]
            },
            &ctx
        )
        .unwrap(),
        json!(false)
    );
    // empty Or = false (identity)
    assert_eq!(
        eval_expr(&Expr::Or { operands: vec![] }, &ctx).unwrap(),
        json!(false)
    );
}

// ──────────────────────────────────────────────────────────────────────────
// Stage 1: existence
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn exists_op() {
    let ctx = json!({ "a": { "b": 1 }, "n": null });
    assert_eq!(
        eval_expr(&Expr::Exists { at: "$.a.b".into() }, &ctx).unwrap(),
        json!(true)
    );
    assert_eq!(
        eval_expr(
            &Expr::Exists {
                at: "$.a.missing".into()
            },
            &ctx
        )
        .unwrap(),
        json!(false)
    );
    // Present-but-null counts as existing (distinct from "missing key")
    assert_eq!(
        eval_expr(&Expr::Exists { at: "$.n".into() }, &ctx).unwrap(),
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
        eval_expr(&Expr::Len { of: path("$.arr") }, &ctx).unwrap(),
        json!(3)
    );
    assert_eq!(
        eval_expr(&Expr::Len { of: path("$.s") }, &ctx).unwrap(),
        json!(5)
    );
    assert_eq!(
        eval_expr(&Expr::Len { of: path("$.o") }, &ctx).unwrap(),
        json!(2)
    );
    // Number / bool / null are not len-able
    assert!(eval_expr(&Expr::Len { of: lit(json!(42)) }, &ctx).is_err());
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
// Schema: snake_case op tag for new variants
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn parse_new_ops_from_json() {
    let json_src = serde_json::json!({
        "op": "and",
        "operands": [
            { "op": "lt", "lhs": { "op": "path", "at": "$.x" }, "rhs": { "op": "lit", "value": 10 } },
            { "op": "exists", "at": "$.flag" }
        ]
    });
    let e: Expr = serde_json::from_value(json_src).unwrap();
    let ctx = json!({ "x": 5, "flag": true });
    assert_eq!(eval_expr(&e, &ctx).unwrap(), json!(true));
}
