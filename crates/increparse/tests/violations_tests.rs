//! Tests for `increparse::run` and contract-violation reporting.

use increparse::prelude::*;

#[test]
fn run_is_the_one_liner_experiment() {
    let settle = pass_fn(|_source: &str, _span, _ctx: &()| Outcome::Done);
    let (tree, report) = run("hello", (settle,), ());

    assert!(report.reached_fixpoint);
    assert_eq!(tree.status(tree.root()), Status::Done);
    assert_eq!(tree.source_rev(), 0);
}

#[test]
fn escaping_child_is_reported_as_violation() {
    // The pass lets its child escape the region it was parsed from.
    let escape = pass_fn(|source: &str, span, _ctx: &()| {
        // Runs past the end of the region: a genuine escape.
        Outcome::one(Span::new(span.start, source.len() + 5, span.rev), ())
    });

    let (tree, report) = run("abcdef", (escape,), ());

    assert_eq!(report.nodes_failed, 1);
    assert_eq!(report.violations.len(), 1);
    let violation = &report.violations[0];
    assert_eq!(violation.node, tree.root());
    assert_eq!(violation.round, 0);
    assert_eq!(violation.pass, Some("PassFn"));
    assert_eq!(violation.span.to_range(), 0..11);
    assert_eq!(violation.kind, ViolationKind::OutsideParent);

    // The message is written for humans, not compilers.
    let message = violation.to_string();
    assert!(message.contains("escapes its parent"), "{message}");

    // The node is failed and left childless, exactly as before.
    assert_eq!(tree.status(tree.root()), Status::Failed);
    assert!(tree.children(tree.root()).is_empty());
}

#[test]
fn stale_revision_child_is_reported() {
    let stale = pass_fn(|_source: &str, span, _ctx: &()| {
        Outcome::one(Span::new(span.start, span.end - 1, span.rev + 1), ())
    });

    let (_, report) = run("abcdef", (stale,), ());

    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].kind, ViolationKind::WrongRevision);
}

#[test]
fn non_shrinking_child_is_reported_when_enforced() {
    let same_span = pass_fn(|_source: &str, span, _ctx: &()| Outcome::one(span, ()));
    let engine = Engine::with_config(
        (same_span,),
        EngineConfig {
            enforce_shrink: true,
            max_rounds: None,
        },
    );
    let mut tree = ParseTree::from_source("abcdef", 0, ());
    let report = engine.run("abcdef", &mut tree, &SerialExecutor, &CancelToken::new());

    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].kind, ViolationKind::NotSmaller);
}

#[test]
fn well_behaved_passes_report_no_violations() {
    let tidy = pass_fn(|_source: &str, span, _ctx: &()| {
        Outcome::one(
            Span::new(span.start, span.end.saturating_sub(1), span.rev),
            (),
        )
    });
    let (_, report) = run("abcdef", (tidy,), ());
    assert!(report.violations.is_empty());
}

#[test]
fn violations_carry_the_pass_name() {
    fn named_scan(source: &str, span: Span, _ctx: &()) -> Outcome<()> {
        Outcome::one(Span::new(span.end, source.len() + 3, span.rev), ())
    }

    let (tree, report) = run("abcdef", (pass_fn(named_scan),), ());
    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].pass, Some("PassFn"));
    // And the tree still survived the incident.
    assert_eq!(tree.status(tree.root()), Status::Failed);
}
