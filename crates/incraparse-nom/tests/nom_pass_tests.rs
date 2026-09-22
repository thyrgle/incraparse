//! Tests for the nom adapter: rebasing correctness, outcome conversion, and
//! — the acceptance gate — reuse-matching across edits with
//! adapter-produced spans.

use incraparse::Span;
use incraparse::{CancelToken, Engine, Outcome, ParseTree, Pass, Schedule, SerialExecutor, Status};
use incraparse_nom::{nom_pass, NomChildren};
use nom::bytes::complete::tag;
use nom::combinator::{map, opt, recognize};
use nom::multi::{many0, many1};
use nom::IResult;
use nom::Parser;
use nom_locate::LocatedSpan;
use std::ops::Range;

type Located<'a> = LocatedSpan<&'a str>;

/// Recognizes `hi` and emits the consumed range as a child.
fn hi_pass_fn(i: Located) -> IResult<Located, NomChildren<()>> {
    let (i, consumed) = recognize(tag("hi")).parse(i)?;
    let end = consumed.fragment().len();
    Ok((i, vec![(0..end, ())]))
}

#[test]
fn rebasing_makes_children_absolute() {
    let pass = nom_pass(hi_pass_fn);
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
    let pass = nom_pass(hi_pass_fn);
    let source = "oh no";
    let outcome = pass.parse(source, Span::new(0, source.len(), 0), &());
    assert!(matches!(outcome, Outcome::Failed));
}

#[test]
fn empty_children_is_done() {
    fn nothing(i: Located) -> IResult<Located, NomChildren<()>> {
        Ok((i, Vec::new()))
    }
    let pass = nom_pass(nothing);
    let outcome = pass.parse("abc", Span::new(0, 3, 0), &());
    assert!(matches!(outcome, Outcome::Done));
}

#[test]
fn multibyte_offsets_rebase_correctly() {
    // The region starts after a 4-byte emoji: relative byte offsets must
    // land on the same characters once rebased.
    fn one_child(i: Located) -> IResult<Located, NomChildren<()>> {
        let start = i.location_offset();
        let (i, _) = tag("éx")(i)?;
        let end = i.location_offset();
        Ok((i, vec![(start..end, ())]))
    }

    let pass = nom_pass(one_child);
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
/// implemented entirely with nom.
fn split_at_marker(i: Located) -> IResult<Located, NomChildren<Option<bool>>> {
    let start = i.location_offset();
    let (i, left_frag) = nom::bytes::complete::take_till(|c| c == '|').parse(i)?;
    let left = Range {
        start,
        end: start + left_frag.fragment().len(),
    };
    let (i, _) = tag("|").parse(i)?;
    let right_start = i.location_offset();
    let (i, right_frag) = nom::combinator::rest.parse(i)?;
    let right = Range {
        start: right_start,
        end: right_start + right_frag.fragment().len(),
    };
    Ok((i, vec![(left, Some(true)), (right, Some(false))]))
}

/// Round 1: settle regions (Done), but only if they contain no `!`.
fn settle_clean(i: Located) -> IResult<Located, NomChildren<Option<bool>>> {
    let (_, fragment) = recognize(many0(nom::character::complete::anychar)).parse(i)?;
    if fragment.contains('!') {
        Err(nom::Err::Error(nom::error::Error::new(
            i,
            nom::error::ErrorKind::Tag,
        )))
    } else {
        Ok((i, Vec::new()))
    }
}

#[test]
fn adapter_spans_reuse_match_across_edits() {
    // Engine + Session + a schedule of two nom-backed passes.
    let mut schedule = Schedule::new();
    schedule.push(nom_pass(split_at_marker));
    schedule.push(nom_pass(settle_clean));
    let engine = Engine::new(schedule);

    let source = "abcdef|ghi!kl";
    let mut tree: ParseTree<Option<bool>> = ParseTree::new(0, Span::new(0, source.len(), 0), None);

    let report = engine.run(source, &mut tree, &SerialExecutor, &CancelToken::new());
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

    // Same-length edit fixing the '!' inside the right half only. The nom
    // pass re-runs on the root and re-emits both children — their rebased
    // spans must match the surviving ones, or incrementalism is broken.
    let edited = "abcdef|ghiXkl";
    let mut schedule = Schedule::new();
    schedule.push(nom_pass(split_at_marker));
    schedule.push(nom_pass(settle_clean));
    let engine = Engine::new(schedule);
    tree.edit(incraparse::Edit::replace(10, 11, 11));
    let report = engine.run(edited, &mut tree, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint, "re-run must settle: {report:?}");

    let kids = tree.children(root);
    assert_eq!(kids[0], left, "left must be reused, not re-created");
    assert_eq!(kids[1], right, "right must be reused, not re-created");
    assert_eq!(tree.status(left), Status::Done);
    assert_eq!(tree.status(right), Status::Done, "right healed to Done");
    assert_eq!(report.nodes_processed, 2, "root + right only");
}

#[test]
fn opt_and_map_combinators_compose() {
    // A pass that parses "abc" repeatedly, tolerating a trailing "!".
    fn abc_chunks(i: Located) -> IResult<Located, NomChildren<()>> {
        let (i, chunks) = many1(map(
            recognize((tag("abc"), opt(tag("!")))),
            |consumed: Located| {
                let start = consumed.location_offset();
                (start..start + consumed.fragment().len(), ())
            },
        ))
        .parse(i)?;
        Ok((i, chunks))
    }

    let pass = nom_pass(abc_chunks);
    let source = "abcabc!abc";
    let outcome = pass.parse(source, Span::new(0, source.len(), 0), &());
    match outcome {
        Outcome::Expand(children) => {
            let spans: Vec<Span> = children.into_iter().map(|(s, _)| s).collect();
            assert_eq!(
                spans,
                vec![Span::new(0, 3, 0), Span::new(3, 7, 0), Span::new(7, 10, 0)]
            );
        }
        other => panic!("expected Expand, got {other:?}"),
    }
}
