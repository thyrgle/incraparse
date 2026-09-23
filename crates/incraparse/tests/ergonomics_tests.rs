//! Tests for the ergonomic constructors: `pass_fn`, `Engine::with`,
//! `from_source`, `Outcome::one`, and the preludes.

use incraparse::prelude::*;

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
