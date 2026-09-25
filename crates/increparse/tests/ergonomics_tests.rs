//! Tests for the ergonomic constructors: `pass_fn`, `Engine::with`,
//! `from_source`, `Outcome::one`, and the preludes.

use increparse::prelude::*;

#[test]
fn pass_fn_creates_working_passes() {
    let mut schedule = Schedule::new();
    schedule.push(pass_fn(|_source: &str, span, _ctx: &()| {
        Outcome::one(span, ())
    }));
    // Round 1: settle the region round 0 created.
    schedule.push(pass_fn(|_source: &str, _span, _ctx: &()| Outcome::Done));

    let engine = Engine::new(schedule);
    let mut tree = ParseTree::from_source("abc", 0, ());
    let report = engine.run("abc", &mut tree, &SerialExecutor, &CancelToken::new());

    assert!(report.reached_fixpoint);
    assert_eq!(tree.status(tree.root()), Status::Expanded);
    assert_eq!(tree.status(tree.children(tree.root())[0]), Status::Done);
}

#[test]
fn pass_fn_captures_state() {
    // Closures can carry state without a newtype.
    let token = "def".to_string();
    let pass = pass_fn(move |source: &str, span, _ctx: &()| {
        if source[span.to_range()].starts_with(&token) {
            Outcome::Done
        } else {
            Outcome::Failed
        }
    });

    let engine = Engine::with((pass,));
    let mut tree = ParseTree::from_source("def x", 0, ());
    engine.run("def x", &mut tree, &SerialExecutor, &CancelToken::new());
    assert_eq!(tree.status(tree.root()), Status::Done);
}

#[test]
fn engine_with_accepts_mixed_pass_tuples() {
    struct CountPass(#[allow(dead_code)] u32);
    impl Pass for CountPass {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            Outcome::one(span, ())
        }
    }

    let a = pass_fn(|_source: &str, _span, _ctx: &()| Outcome::<()>::Failed);
    let b = CountPass(1);
    let c = pass_fn(|_source: &str, _span, _ctx: &()| Outcome::<()>::Done);

    // Three different pass types, one tuple, no boxing on the user side.
    let engine = Engine::with((a, b, c));
    let mut tree = ParseTree::from_source("abcdef", 0, ());
    let report = engine.run("abcdef", &mut tree, &SerialExecutor, &CancelToken::new());

    assert!(report.reached_fixpoint);
    assert_eq!(report.rounds_run, 3);
}

#[test]
fn engine_with_accepts_boxed_vec() {
    let passes: Vec<Box<dyn Pass<Ctx = ()> + Send + Sync>> =
        vec![Box::new(pass_fn(|_s: &str, _p, _c: &()| Outcome::Done))];
    let engine = Engine::with(passes);

    let mut tree = ParseTree::from_source("abc", 0, ());
    let report = engine.run("abc", &mut tree, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);
    assert_eq!(tree.status(tree.root()), Status::Done);
}

#[test]
fn outcome_one_expands_single_child() {
    let pass = pass_fn(|_source: &str, _span, _ctx: &()| Outcome::one(Span::new(1, 2, 0), ()));
    let mut schedule = Schedule::new();
    schedule.push(pass);
    let engine = Engine::new(schedule);

    let mut tree = ParseTree::from_source("abc", 0, ());
    engine.run("abc", &mut tree, &SerialExecutor, &CancelToken::new());

    let kid = tree.children(tree.root())[0];
    assert_eq!(tree.span(kid).to_range(), 1..2);
}

#[test]
fn from_source_covers_whole_text() {
    let tree = ParseTree::<()>::from_source("hello", 7, ());
    assert_eq!(tree.span(tree.root()).to_range(), 0..5);
    assert_eq!(tree.source_rev(), 7);

    let session = Session::<()>::from_source("hello", 7, ());
    assert_eq!(session.tree().span(session.tree().root()).to_range(), 0..5);
}

#[test]
fn node_at_finds_the_deepest_containing_node() {
    // root 0..10 -> two children -> inner 1..4 inside the first child
    fn splitter(source: &str, span: Span, _ctx: &()) -> Outcome<()> {
        if span.len() >= 3 {
            return Outcome::one(Span::new(span.start + 1, span.end - 1, span.rev), ());
        }
        let _ = source;
        Outcome::Done
    }
    // Chain: root 0..10 -> 1..9 -> 2..8 -> 3..7 -> 4..6 (Done).
    let (tree, report) = run(
        "abcdefghij",
        (
            pass_fn(splitter),
            pass_fn(splitter),
            pass_fn(splitter),
            pass_fn(splitter),
            pass_fn(splitter),
        ),
        (),
    );
    assert!(report.reached_fixpoint, "{report:?}");

    let root = tree.root();
    let mid = tree.children(root)[0]; // 1..9
    let inner = tree.children(mid)[0]; // 2..8
    let deeper = tree.children(inner)[0]; // 3..7
    let leaf = tree.children(deeper)[0]; // 4..6

    assert_eq!(tree.node_at(2), Some(inner), "deepest containing node wins");
    assert_eq!(
        tree.node_at(1),
        Some(mid),
        "mid covers 1, inner starts at 2"
    );
    assert_eq!(
        tree.node_at(6),
        Some(deeper),
        "3..7 contains 6, leaf 4..6 doesn't"
    );
    assert_eq!(
        tree.node_at(7),
        Some(inner),
        "half-open: 2..8 covers up to 7"
    );
    assert_eq!(tree.node_at(5), Some(leaf));
    assert_eq!(tree.node_at(9), Some(root), "only the root covers 9");
    assert_eq!(tree.node_at(0), Some(root));
    assert_eq!(tree.node_at(10), None);
    assert_eq!(tree.node_at(99), None);
    assert_eq!(tree.status(leaf), Status::Done);
}
