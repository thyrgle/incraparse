//! Tests for the chumsky adapter: rebasing correctness, outcome conversion,
//! and the acceptance gate — reuse-matching across edits with
//! adapter-produced spans.

use chumsky::prelude::*;
use increparse::{
    CancelToken, Engine, Outcome, ParseTree, Pass, Schedule, SerialExecutor, Span, Status,
};
use increparse_chumsky::{chumsky_pass, ChumChildren};

/// Recognizes `hi` and emits the consumed span as a child.
fn hi_pass_fn(slice: &str) -> chumsky::ParseResult<ChumChildren<()>, Rich<'_, char>> {
    fn parser<'a>() -> impl Parser<'a, &'a str, ChumChildren<()>, extra::Err<Rich<'a, char>>> {
        // `parse` demands whole-input consumption, so tolerate the tail.
        just("hi")
            .map_with(|_out, e| vec![(e.span(), ())])
            .then_ignore(any().repeated())
    }
    parser().parse(slice)
}

#[test]
fn rebasing_makes_children_absolute() {
    let pass = chumsky_pass(hi_pass_fn);
    let source = "..hi...";
    let outcome = pass.parse(source, Span::new(2, 7, 3), &());

    match outcome {
        Outcome::Expand(children) => {
            assert_eq!(children.len(), 1);
            let (span, ctx) = &children[0];
            assert_eq!(*span, Span::new(2, 4, 3), "absolute, parent's revision");
            assert_eq!(&source[span.to_range()], "hi");
            assert_eq!(*ctx, ());
        }
        other => panic!("expected Expand, got {other:?}"),
    }
}

#[test]
fn parse_error_becomes_failed() {
    let pass = chumsky_pass(hi_pass_fn);
    let source = "oh no";
    let outcome = pass.parse(source, Span::new(0, source.len(), 0), &());
    assert!(matches!(outcome, Outcome::Failed));
}

#[test]
fn empty_children_is_done() {
    fn nothing(slice: &str) -> chumsky::ParseResult<ChumChildren<()>, Rich<'_, char>> {
        fn parser<'a>() -> impl Parser<'a, &'a str, ChumChildren<()>, extra::Err<Rich<'a, char>>> {
            any().repeated().to(Vec::new())
        }
        parser().parse(slice)
    }
    let pass = chumsky_pass(nothing);
    let outcome = pass.parse("abc", Span::new(0, 3, 0), &());
    assert!(matches!(outcome, Outcome::Done));
}

#[test]
fn multibyte_offsets_rebase_correctly() {
    fn one_child(slice: &str) -> chumsky::ParseResult<ChumChildren<()>, Rich<'_, char>> {
        just::<&str, &str, extra::Err<Rich<char>>>("éx")
            .map_with(|_out: &str, e| {
                let span: SimpleSpan = e.span();
                vec![(span, ())]
            })
            .then_ignore(any().repeated())
            .parse(slice)
    }

    let pass = chumsky_pass(one_child);
    let source = "😀..éx..";
    // Region begins at "éx" (after the 4-byte emoji and two dots).
    let outcome = pass.parse(source, Span::new(6, source.len(), 0), &());
    match outcome {
        Outcome::Expand(children) => {
            let (span, _) = &children[0];
            assert_eq!(span.start, 6);
            assert_eq!(span.end, 9);
            assert_eq!(&source[span.to_range()], "éx");
        }
        other => panic!("expected Expand, got {other:?}"),
    }
}

/// Round 0: split the file at the marker `|` into left and right halves,
/// implemented with chumsky.
fn split_marker_pass_fn(
    slice: &str,
) -> chumsky::ParseResult<ChumChildren<Option<bool>>, Rich<'_, char>> {
    fn parser<'a>(
    ) -> impl Parser<'a, &'a str, ChumChildren<Option<bool>>, extra::Err<Rich<'a, char>>> {
        any().repeated().to_slice().map_with(|slice: &str, e| {
            let whole: SimpleSpan = e.span();
            match slice.find('|') {
                Some(bar) => vec![
                    ((whole.start..whole.start + bar).into(), Some(true)),
                    ((whole.start + bar + 1..whole.end).into(), Some(false)),
                ],
                None => Vec::new(),
            }
        })
    }
    parser().parse(slice)
}

/// Round 1: settle regions (Done), but only if they contain no `!`.
fn settle_clean_pass_fn(
    slice: &str,
) -> chumsky::ParseResult<ChumChildren<Option<bool>>, Rich<'_, char>> {
    fn parser<'a>(
    ) -> impl Parser<'a, &'a str, ChumChildren<Option<bool>>, extra::Err<Rich<'a, char>>> {
        any().repeated().to_slice().validate(|s: &str, e, emitter| {
            if s.contains('!') {
                emitter.emit(Rich::custom(e.span(), "bang"));
            }
            Vec::new()
        })
    }
    parser().parse(slice)
}

#[test]
fn adapter_spans_reuse_match_across_edits() {
    let build = || {
        let mut schedule = Schedule::new();
        schedule.push(chumsky_pass(split_marker_pass_fn));
        schedule.push(chumsky_pass(settle_clean_pass_fn));
        Engine::new(schedule)
    };

    let source = "abcdef|ghi!kl";
    let mut tree: ParseTree<Option<bool>> = ParseTree::new(0, Span::new(0, source.len(), 0), None);

    let report = build().run(source, &mut tree, &SerialExecutor, &CancelToken::new());
    assert!(
        report.reached_fixpoint,
        "initial run must settle: {report:?}"
    );

    let root = tree.root();
    let left = tree.children(root)[0];
    let right = tree.children(root)[1];
    assert_eq!(tree.status(left), Status::Done);
    assert_eq!(
        tree.status(right),
        Status::Failed,
        "right contains '!' initially"
    );
    assert_eq!(tree.span(left).to_range(), 0..6);
    assert_eq!(tree.span(right).to_range(), 7..13);

    // Same-length edit fixing the '!' inside the right half only.
    let edited = "abcdef|ghiXkl";
    tree.edit(increparse::Edit::replace(10, 11, 11));
    let report = build().run(edited, &mut tree, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint, "re-run must settle: {report:?}");

    let kids = tree.children(root);
    assert_eq!(kids[0], left, "left must be reused, not re-created");
    assert_eq!(kids[1], right, "right must be reused, not re-created");
    assert_eq!(tree.status(left), Status::Done);
    assert_eq!(tree.status(right), Status::Done, "right healed to Done");
    assert_eq!(report.nodes_processed, 2, "root + right only");
}
