use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use incraparse::{
    CancelToken, Engine, Outcome, Pass, Schedule, SerialExecutor, Session, Span, Status,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ctx {
    File,
    Left,
    Right { head: char },
}

/// Round 0: splits the file at byte 4 into a fixed left half and a right
/// half whose context embeds the source byte at the split point.
#[derive(Clone)]
struct SplitAtFour {
    calls: Arc<AtomicUsize>,
}

impl Pass for SplitAtFour {
    type Ctx = Ctx;

    fn parse(&self, source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !matches!(ctx, Ctx::File) {
            return Outcome::Failed;
        }
        let split = (span.start + 4).min(span.end);
        let head = source[split..].chars().next().unwrap_or('?');
        Outcome::Expand(vec![
            (Span::new(span.start, split, span.rev), Ctx::Left),
            (Span::new(split, span.end, span.rev), Ctx::Right { head }),
        ])
    }
}

/// Round 1: accepts regions as done.
#[derive(Clone)]
struct Settle {
    calls: Arc<AtomicUsize>,
}

impl Pass for Settle {
    type Ctx = Ctx;

    fn parse(&self, _source: &str, _span: Span, ctx: &Ctx) -> Outcome<Ctx> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match ctx {
            Ctx::File => Outcome::Failed,
            _ => Outcome::Done,
        }
    }
}

/// Round 1 variant: fails any region containing `!`.
#[derive(Clone)]
struct NoBang {
    calls: Arc<AtomicUsize>,
}

impl Pass for NoBang {
    type Ctx = Ctx;

    fn parse(&self, source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(ctx, Ctx::File) {
            return Outcome::Failed;
        }
        if source[span.to_range()].contains('!') {
            Outcome::Failed
        } else {
            Outcome::Done
        }
    }
}

fn make_session(source: &str) -> (Session<Ctx>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let split_calls = Arc::new(AtomicUsize::new(0));
    let settle_calls = Arc::new(AtomicUsize::new(0));

    let mut schedule = Schedule::new();
    schedule.push(SplitAtFour {
        calls: split_calls.clone(),
    });
    schedule.push(Settle {
        calls: settle_calls.clone(),
    });
    let engine = Engine::new(schedule);

    let mut session: Session<Ctx> = Session::new(0, Span::new(0, source.len(), 0), Ctx::File);
    let report = session.run(&engine, source, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);

    (session, split_calls, settle_calls)
}

#[test]
fn untouched_sibling_is_reused_with_same_identity() {
    let source = "abcdefghij";
    let (mut session, split_calls, settle_calls) = make_session(source);

    let root = session.tree().root();
    let left = session.tree().children(root)[0];
    let right = session.tree().children(root)[1];
    assert_eq!(split_calls.load(Ordering::SeqCst), 1);
    assert_eq!(settle_calls.load(Ordering::SeqCst), 2);

    // Same-length edit at byte 7 (inside the right half only).
    let edited = "abcdefgij";
    session.edit(incraparse::Edit::replace(7, 8, 8));
    assert_eq!(session.revision(), 1);

    let engine = Engine::new({
        let mut s = Schedule::new();
        s.push(SplitAtFour {
            calls: split_calls.clone(),
        });
        s.push(Settle {
            calls: settle_calls.clone(),
        });
        s
    });
    let report = session.run(&engine, edited, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);

    // Both regions matched after re-expansion: same ids, left never re-parsed.
    let kids = session.tree().children(root);
    assert_eq!(kids, &[left, right]);
    assert_eq!(session.tree().status(left), Status::Done);
    assert_eq!(session.tree().status(right), Status::Done);
    assert_eq!(split_calls.load(Ordering::SeqCst), 2);
    assert_eq!(settle_calls.load(Ordering::SeqCst), 3);
    assert_eq!(session.tree().span(right).to_range(), 4..10);
    assert_eq!(session.tree().ctx(right), &Ctx::Right { head: 'e' });
}

#[test]
fn insert_at_end_reuses_left_and_recreates_shifted_right() {
    let source = "abcdefghij";
    let (mut session, split_calls, settle_calls) = make_session(source);

    let root = session.tree().root();
    let left = session.tree().children(root)[0];
    let right = session.tree().children(root)[1];

    // Insert "XYZ" at the end of the file.
    let edited = "abcdefghijXYZ";
    session.edit(incraparse::Edit::insert(10, 3));
    session.run(
        &Engine::new({
            let mut s = Schedule::new();
            s.push(SplitAtFour {
                calls: split_calls.clone(),
            });
            s.push(Settle {
                calls: settle_calls.clone(),
            });
            s
        }),
        edited,
        &SerialExecutor,
        &CancelToken::new(),
    );

    // The left region was untouched by the edit and is reused as-is.
    let kids = session.tree().children(root);
    assert_eq!(kids[0], left);
    assert_eq!(session.tree().status(left), Status::Done);
    assert_eq!(session.tree().span(left).to_range(), 0..4);

    // The right half ends at the insertion point, so it absorbs the appended
    // bytes: same identity, invalidated and re-parsed over the grown span.
    assert_eq!(kids[1], right);
    assert_eq!(session.tree().span(kids[1]).to_range(), 4..13);
    assert_eq!(session.tree().status(kids[1]), Status::Done);
    assert_eq!(session.tree().status_counts().total(), 3);
    assert!(session.tree().nodes().any(|id| id == right));
    assert_eq!(settle_calls.load(Ordering::SeqCst), 3);
}

#[test]
fn context_change_recreates_node_and_detaches_stale_subtree() {
    let source = "abcdefghij";
    let (mut session, split_calls, settle_calls) = make_session(source);

    let root = session.tree().root();
    let left = session.tree().children(root)[0];
    let right = session.tree().children(root)[1];

    // Replace the byte at the split point: the right region's context
    // (its `head` char) changes, so it cannot be reused.
    let edited = "abcdxfghi";
    session.edit(incraparse::Edit::replace(4, 5, 5));
    session.run(
        &Engine::new({
            let mut s = Schedule::new();
            s.push(SplitAtFour {
                calls: split_calls.clone(),
            });
            s.push(Settle {
                calls: settle_calls.clone(),
            });
            s
        }),
        edited,
        &SerialExecutor,
        &CancelToken::new(),
    );

    let kids = session.tree().children(root);
    assert_eq!(kids[0], left);
    assert_ne!(kids[1], right);
    assert_eq!(session.tree().ctx(kids[1]), &Ctx::Right { head: 'x' });
    assert_eq!(session.tree().status(kids[1]), Status::Done);
    assert_eq!(session.tree().status_counts().total(), 3);
    assert_eq!(session.tree().len(), 3);
    assert!(!session.tree().nodes().any(|id| id == right));
    assert_eq!(settle_calls.load(Ordering::SeqCst), 3);
}

#[test]
fn edit_heals_previously_failed_region() {
    let source = "abcd!efghi";
    let nobang_calls = Arc::new(AtomicUsize::new(0));
    let split_calls = Arc::new(AtomicUsize::new(0));

    let mut schedule = Schedule::new();
    schedule.push(SplitAtFour {
        calls: split_calls.clone(),
    });
    schedule.push(NoBang {
        calls: nobang_calls.clone(),
    });
    let engine = Engine::new(schedule);

    let mut session: Session<Ctx> = Session::new(0, Span::new(0, source.len(), 0), Ctx::File);
    let report = session.run(&engine, source, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);

    let root = session.tree().root();
    let left = session.tree().children(root)[0];
    let right = session.tree().children(root)[1];
    assert_eq!(session.tree().status(left), Status::Done);
    assert_eq!(session.tree().status(right), Status::Failed);
    assert_eq!(session.tree().attempts(right), 1);

    // The edit removes the offending `!` (the right region's head char), so
    // the region is recreated fresh — and now parses.
    let edited = "abcd.efghi";
    session.edit(incraparse::Edit::replace(4, 5, 5));
    let report = session.run(&engine, edited, &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);

    let kids = session.tree().children(root);
    assert_eq!(kids[0], left);
    assert_ne!(kids[1], right);
    assert!(!session.tree().nodes().any(|id| id == right));
    assert_eq!(session.tree().status(kids[1]), Status::Done);
    assert_eq!(session.tree().attempts(kids[1]), 0);
    assert_eq!(nobang_calls.load(Ordering::SeqCst), 3);
}

#[test]
fn session_revision_tracks_edits() {
    let source = "abcdefghij";
    let (mut session, _split, _settle) = make_session(source);

    assert_eq!(session.revision(), 0);
    session.edit(incraparse::Edit::insert(5, 2));
    assert_eq!(session.revision(), 1);
    session.edit(incraparse::Edit::delete(2, 3));
    assert_eq!(session.revision(), 2);
    assert_eq!(session.tree().source_rev(), 2);
}

#[test]
fn edit_keeps_engine_contract_violations_detected() {
    // After an edit the root must still validate produced children: a pass
    // that escapes its parent is rejected exactly as in a fresh run.
    struct EscapeOnFile;
    impl Pass for EscapeOnFile {
        type Ctx = Ctx;
        fn parse(&self, _source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
            if !matches!(ctx, Ctx::File) {
                return Outcome::Failed;
            }
            Outcome::Expand(vec![(
                Span::new(span.start, span.end + 1, span.rev),
                Ctx::Left,
            )])
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(EscapeOnFile);
    let engine = Engine::new(schedule);

    let mut session: Session<Ctx> = Session::new(0, Span::new(0, 10, 0), Ctx::File);
    session.run(&engine, "abcdefghij", &SerialExecutor, &CancelToken::new());
    assert_eq!(session.tree().status(session.tree().root()), Status::Failed);

    session.edit(incraparse::Edit::insert(5, 1));
    let report = session.run(&engine, "abcdefghijk", &SerialExecutor, &CancelToken::new());
    assert!(report.reached_fixpoint);
    assert_eq!(session.tree().status(session.tree().root()), Status::Failed);
    assert_eq!(report.nodes_failed, 1);
}
