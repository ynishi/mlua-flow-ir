#![cfg(feature = "mlua")]

use mlua::Lua;
use mlua_flow_ir::module;

// ──────────────────────────────────────────────────────────────────────────
// Setup helper
// ──────────────────────────────────────────────────────────────────────────

fn setup_lua() -> Lua {
    let lua = Lua::new();
    let m = module(&lua).unwrap();
    lua.globals().set("flow", m).unwrap();
    lua
}

// ──────────────────────────────────────────────────────────────────────────
// Module surface
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn module_exposes_version_string() {
    let lua = setup_lua();
    let v: String = lua.load("return flow.version").eval().unwrap();
    assert_eq!(v, env!("CARGO_PKG_VERSION"));
}

#[test]
fn module_exposes_eval_function() {
    let lua = setup_lua();
    let t: String = lua.load("return type(flow.eval)").eval().unwrap();
    assert_eq!(t, "function");
}

// ──────────────────────────────────────────────────────────────────────────
// flow.eval — Step
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn lua_eval_simple_step_uppercase() {
    let lua = setup_lua();
    let result: mlua::Value = lua
        .load(
            r#"
        local node = {
            kind = "step",
            ref = "uppercase",
            ["in"] = { op = "path", at = "$.input" },
            out = { op = "path", at = "$.output" },
        }
        local function dispatcher(r, input)
            if r == "uppercase" then
                return string.upper(input)
            end
        end
        return flow.eval(node, { input = "hello" }, dispatcher)
    "#,
        )
        .eval()
        .unwrap();

    let result_table: mlua::Table = match result {
        mlua::Value::Table(t) => t,
        _ => panic!("expected table"),
    };
    let output: String = result_table.get("output").unwrap();
    assert_eq!(output, "HELLO");
}

// ──────────────────────────────────────────────────────────────────────────
// flow.eval — Seq
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn lua_eval_seq_double_chain() {
    let lua = setup_lua();
    let result: mlua::Value = lua
        .load(
            r#"
        local node = {
            kind = "seq",
            children = {
                { kind = "step", ref = "double", ["in"] = { op = "path", at = "$.n" },  out = { op = "path", at = "$.a" } },
                { kind = "step", ref = "double", ["in"] = { op = "path", at = "$.a" },  out = { op = "path", at = "$.b" } },
            },
        }
        local function dispatcher(r, input)
            if r == "double" then
                return input * 2
            end
        end
        return flow.eval(node, { n = 3 }, dispatcher)
    "#,
        )
        .eval()
        .unwrap();

    let t: mlua::Table = match result {
        mlua::Value::Table(t) => t,
        _ => panic!("expected table"),
    };
    let b: i64 = t.get("b").unwrap();
    assert_eq!(b, 12, "3 * 2 * 2 = 12");
}

// ──────────────────────────────────────────────────────────────────────────
// flow.eval — Branch
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn lua_eval_branch_then_path() {
    let lua = setup_lua();
    let result: mlua::Value = lua
        .load(
            r#"
        local node = {
            kind = "branch",
            cond = {
                op = "eq",
                lhs = { op = "path", at = "$.flag" },
                rhs = { op = "lit", value = true },
            },
            ["then"] = {
                kind = "step", ref = "yes_path",
                ["in"] = { op = "lit", value = false },
                out = { op = "path", at = "$.result" },
            },
            ["else"] = {
                kind = "step", ref = "no_path",
                ["in"] = { op = "lit", value = false },
                out = { op = "path", at = "$.result" },
            },
        }
        local function dispatcher(r, _i)
            if r == "yes_path" then return "YES" end
            if r == "no_path" then return "NO" end
        end
        return flow.eval(node, { flag = true }, dispatcher)
    "#,
        )
        .eval()
        .unwrap();

    let t: mlua::Table = match result {
        mlua::Value::Table(t) => t,
        _ => panic!("expected table"),
    };
    let r: String = t.get("result").unwrap();
    assert_eq!(r, "YES");
}

#[test]
fn lua_eval_branch_else_path() {
    let lua = setup_lua();
    let result: mlua::Value = lua
        .load(
            r#"
        local node = {
            kind = "branch",
            cond = {
                op = "eq",
                lhs = { op = "path", at = "$.flag" },
                rhs = { op = "lit", value = true },
            },
            ["then"] = {
                kind = "step", ref = "yes_path",
                ["in"] = { op = "lit", value = false },
                out = { op = "path", at = "$.result" },
            },
            ["else"] = {
                kind = "step", ref = "no_path",
                ["in"] = { op = "lit", value = false },
                out = { op = "path", at = "$.result" },
            },
        }
        local function dispatcher(r, _i)
            if r == "yes_path" then return "YES" end
            if r == "no_path" then return "NO" end
        end
        return flow.eval(node, { flag = false }, dispatcher)
    "#,
        )
        .eval()
        .unwrap();

    let t: mlua::Table = match result {
        mlua::Value::Table(t) => t,
        _ => panic!("expected table"),
    };
    let r: String = t.get("result").unwrap();
    assert_eq!(r, "NO");
}

// ──────────────────────────────────────────────────────────────────────────
// Dispatcher error propagation
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn lua_eval_dispatcher_returning_nil_for_unknown_ref_errors() {
    let lua = setup_lua();
    // dispatcher が nil 返す = Rust 側 Value::Null 化 → write_path で $.r に Null 書き込み = OK
    // 別 case: dispatcher が error throw (= Lua error) → Rust 側で DispatcherError propagation
    let result = lua
        .load(
            r#"
        local node = {
            kind = "step", ref = "explode",
            ["in"] = { op = "lit", value = false },
            out = { op = "path", at = "$.r" },
        }
        local function dispatcher(_r, _i)
            error("intentional lua error")
        end
        return flow.eval(node, {}, dispatcher)
    "#,
        )
        .eval::<mlua::Value>();

    assert!(result.is_err(), "expect dispatcher error to propagate");
}

// ──────────────────────────────────────────────────────────────────────────
// call_extern via externs table (canonical opts.externs parity)
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn lua_eval_call_extern_resolves_lua_fn() {
    let lua = setup_lua();
    // extern の実体は任意の pure Lua function = LuaScript 直実行 Hatch。
    // UCB1 で必要な sqrt / ln をそのまま whitelist して IR から呼ぶ。
    let result: mlua::Value = lua
        .load(
            r#"
        local node = {
            kind = "assign",
            at = { op = "path", at = "$.score" },
            value = {
                op = "call_extern", ref = "ucb.bonus",
                args = {
                    { op = "path", at = "$.total" },
                    { op = "path", at = "$.n" },
                },
            },
        }
        local externs = {
            ["ucb.bonus"] = function(total, n)
                return math.sqrt(2 * math.log(total + 1) / n)
            end,
        }
        local function dispatcher(_r, _i) end
        return flow.eval(node, { total = 0, n = 1 }, dispatcher, externs)
    "#,
        )
        .eval()
        .unwrap();

    let t: mlua::Table = match result {
        mlua::Value::Table(t) => t,
        _ => panic!("expected table"),
    };
    let score: f64 = t.get("score").unwrap();
    assert!(score.abs() < 1e-12, "sqrt(2*ln(1)/1) = 0, got {score}");
}

#[test]
fn lua_eval_call_extern_unregistered_errors() {
    let lua = setup_lua();
    let result = lua
        .load(
            r#"
        local node = {
            kind = "assign",
            at = { op = "path", at = "$.x" },
            value = { op = "call_extern", ref = "nope", args = {} },
        }
        local function dispatcher(_r, _i) end
        return flow.eval(node, {}, dispatcher, {})
    "#,
        )
        .eval::<mlua::Value>();
    assert!(result.is_err(), "unregistered extern must raise");
}

#[test]
fn lua_eval_call_extern_without_externs_table_errors() {
    let lua = setup_lua();
    // 4th arg 省略 = NoExterns → canonical "requires opts.externs" parity
    let result = lua
        .load(
            r#"
        local node = {
            kind = "assign",
            at = { op = "path", at = "$.x" },
            value = { op = "call_extern", ref = "f", args = {} },
        }
        local function dispatcher(_r, _i) end
        return flow.eval(node, {}, dispatcher)
    "#,
        )
        .eval::<mlua::Value>();
    assert!(result.is_err(), "call_extern without externs must raise");
}

// ──────────────────────────────────────────────────────────────────────────
// canonical wire format: gte / lte tags parse via Lua bridge
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn lua_eval_gte_lte_wire_tags() {
    let lua = setup_lua();
    let result: mlua::Value = lua
        .load(
            r#"
        local node = {
            kind = "branch",
            cond = {
                op = "and",
                args = {
                    { op = "gte", lhs = { op = "lit", value = 3 }, rhs = { op = "lit", value = 3 } },
                    { op = "lte", lhs = { op = "lit", value = 2 }, rhs = { op = "lit", value = 3 } },
                },
            },
            ["then"] = {
                kind = "assign",
                at = { op = "path", at = "$.r" },
                value = { op = "lit", value = "ok" },
            },
            ["else"] = {
                kind = "assign",
                at = { op = "path", at = "$.r" },
                value = { op = "lit", value = "ng" },
            },
        }
        local function dispatcher(_r, _i) end
        return flow.eval(node, {}, dispatcher)
    "#,
        )
        .eval()
        .unwrap();

    let t: mlua::Table = match result {
        mlua::Value::Table(t) => t,
        _ => panic!("expected table"),
    };
    let r: String = t.get("r").unwrap();
    assert_eq!(r, "ok");
}
