//! In-crate tests: the Lua outcome protocol, deep-equality contexts, and
//! incremental reuse with Lua-defined passes.

use increparse::{CancelToken, ParseTree, SerialExecutor, Span, Status};
use increparse_lsp::Language;
use increparse_lua::{LuaCtx, LuaLanguage};
use mlua::Lua;

fn test_language() -> LuaLanguage {
    LuaLanguage::from_path("examples/minilang.lua").expect("fixture config loads")
}

fn eval(lua: &Lua, code: &str) -> mlua::Result<mlua::Value> {
    lua.load(code).eval()
}

#[test]
fn config_from_fixture_loads() {
    let language = test_language();
    assert_eq!(language.name(), "minilang");
    assert!(language.supports_symbols());
}

#[test]
fn config_validation_rejects_bad_definitions() {
    let lua = Lua::new();

    // Not a table.
    assert!(LuaLanguage::from_config(lua.clone(), eval(&lua, "42").unwrap()).is_err());

    // Missing passes.
    assert!(LuaLanguage::from_config(
        lua.clone(),
        eval(&lua, "{ name = 'x', root_ctx = {} }").unwrap()
    )
    .is_err());

    // Empty passes.
    assert!(LuaLanguage::from_config(
        lua.clone(),
        eval(&lua, "{ name = 'x', root_ctx = {}, passes = {} }").unwrap()
    )
    .is_err());

    // Passes containing non-functions.
    assert!(LuaLanguage::from_config(
        lua.clone(),
        eval(
            &lua,
            "{ name = 'x', root_ctx = {}, passes = { function() end, 42 } }"
        )
        .unwrap()
    )
    .is_err());
}

#[test]
fn outcome_protocol_expands() {
    let lua = Lua::new();
    let config = eval(
        &lua,
        r#"
        {
          name = "t",
          root_ctx = {},
          passes = {
            function(source, span, ctx)
              return { expand = { { start = 0, ["end"] = 3, ctx = { A = true } } } }
            end,
          },
        }
        "#,
    )
    .unwrap();
    let language = LuaLanguage::from_config(lua, config).unwrap();

    let engine = language.engine();
    let mut tree: ParseTree<LuaCtx> = ParseTree::new(0, Span::new(0, 10, 0), language.root_ctx());
    engine.run(
        "hello worl",
        &mut tree,
        &SerialExecutor,
        &CancelToken::new(),
    );

    assert_eq!(tree.status(tree.root()), increparse::Status::Expanded);
    let kid = tree.children(tree.root())[0];
    assert_eq!(tree.span(kid).to_range(), 0..3);
    // The child context is a Lua table: deep-equal to { A = true }.
    let expected = eval(language.lua(), "return { A = true }").unwrap();
    assert_eq!(*tree.ctx(kid), LuaCtx(expected));
}

#[test]
fn outcome_sentinels() {
    let lua = Lua::new();

    for (code, expected) in [
        ("return \"done\"", Status::Done),
        ("return { done = true }", Status::Failed), // table without expand: failed
        ("return \"failed\"", Status::Failed),
        ("return nil", Status::Failed),
    ] {
        let config = eval(
            &lua,
            &format!(
                r#"
                {{
                  name = "t",
                  root_ctx = {{}},
                  passes = {{ function(source, span, ctx) {code} end }},
                }}
                "#
            ),
        )
        .unwrap();
        let language = LuaLanguage::from_config(lua.clone(), config).unwrap();
        let engine = language.engine();
        let mut tree: ParseTree<LuaCtx> =
            ParseTree::new(0, Span::new(0, 3, 0), language.root_ctx());
        engine.run("abc", &mut tree, &SerialExecutor, &CancelToken::new());
        assert_eq!(tree.status(tree.root()), expected, "case: {code}");
    }
}

#[test]
fn thrown_errors_become_failed_not_crashes() {
    let lua = Lua::new();
    let config = eval(
        &lua,
        r#"
        {
          name = "t",
          root_ctx = {},
          passes = {
            function(source, span, ctx) error("boom") end,
          },
        }
        "#,
    )
    .unwrap();
    let language = LuaLanguage::from_config(lua, config).unwrap();
    let engine = language.engine();
    let mut tree: ParseTree<LuaCtx> = ParseTree::new(0, Span::new(0, 3, 0), language.root_ctx());
    let report = engine.run("abc", &mut tree, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);
    assert_eq!(tree.status(tree.root()), Status::Failed);
}

#[test]
fn deep_equality_gates_reuse() {
    let language = test_language();
    let engine = language.engine();

    let source = "def add(a, b) { return a + b; }\ndef zed(q) { return q; }";
    let mut tree: ParseTree<LuaCtx> =
        ParseTree::new(0, Span::new(0, source.len(), 0), language.root_ctx());
    let report = engine.run(source, &mut tree, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);

    let root = tree.root();
    let add = tree.children(root)[0];
    let zed = tree.children(root)[1];

    // Same-length edit inside add's return only: both functions must be
    // reused (deep-equal contexts), and only add's chain re-parses.
    let edited = "def add(a, b) { return b + a; }\ndef zed(q) { return q; }";
    tree.edit(increparse::Edit::replace(19, 20, 20));
    let report = engine.run(edited, &mut tree, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);

    let kids = tree.children(root);
    assert_eq!(kids[0], add, "add reused");
    assert_eq!(kids[1], zed, "zed reused");
    assert_eq!(tree.status(kids[1]), Status::Expanded, "zed untouched");
    let zed_return = tree.children(kids[1])[0];
    assert_eq!(
        tree.status(zed_return),
        Status::Done,
        "zed's return healed-to-done state intact"
    );
    assert_eq!(report.nodes_processed, 3, "root + add + its return only");
}

#[test]
fn lua_ctx_deep_equality() {
    let lua = Lua::new();
    let a = LuaCtx(eval(&lua, "return { F = { name = 'x', params = { 'a', 'b' } } }").unwrap());
    let b = LuaCtx(eval(&lua, "return { F = { name = 'x', params = { 'a', 'b' } } }").unwrap());
    let c = LuaCtx(eval(&lua, "return { F = { name = 'x', params = { 'a' } } }").unwrap());
    let d = LuaCtx(eval(&lua, "return { F = { name = 'x' } }").unwrap());

    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_ne!(a, d);
    assert_eq!(LuaCtx(mlua::Value::Nil), LuaCtx(mlua::Value::Nil));
    assert_eq!(
        LuaCtx(eval(&lua, "return 1").unwrap()),
        LuaCtx(eval(&lua, "return 1.0").unwrap())
    );
}
