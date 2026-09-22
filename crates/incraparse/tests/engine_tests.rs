use std::sync::atomic::{AtomicUsize, Ordering};

use incraparse::{
    CancelToken, Engine, EngineConfig, Executor, Job, NodeId, Outcome, ParseTree, Pass, RunReport,
    Schedule, SerialExecutor, Span, Status,
};

fn root(source: &str) -> ParseTree<()> {
    ParseTree::new(0, Span::new(0, source.len(), 0), ())
}

fn run_rounds<C: Clone + PartialEq + Send + 'static>(
    schedule: Schedule<C>,
    source: &str,
    tree: &mut ParseTree<C>,
) -> RunReport {
    let engine = Engine::new(schedule);
    engine.run(source, tree, &SerialExecutor, &CancelToken::new())
}

fn done_pass() -> impl Pass<Ctx = ()> {
    struct Done;
    impl Pass for Done {
        type Ctx = ();
        fn parse(&self, _source: &str, _span: Span, _ctx: &()) -> Outcome<()> {
            Outcome::Done
        }
    }
    Done
}

fn fail_pass() -> impl Pass<Ctx = ()> {
    struct Fail;
    impl Pass for Fail {
        type Ctx = ();
        fn parse(&self, _source: &str, _span: Span, _ctx: &()) -> Outcome<()> {
            Outcome::Failed
        }
    }
    Fail
}

#[test]
fn span_geometry() {
    let outer = Span::new(10, 20, 3);
    assert_eq!(outer.len(), 10);
    assert!(!outer.is_empty());

    assert!(outer.contains(&Span::new(10, 15, 3)));
    assert!(outer.contains(&Span::new(15, 20, 3)));
    assert!(!outer.contains(&Span::new(5, 15, 3)));
    assert!(!outer.contains(&Span::new(10, 25, 3)));

    assert!(outer.overlaps(&Span::new(19, 30, 3)));
    assert!(!outer.overlaps(&Span::new(20, 30, 3)));
    assert!(!outer.overlaps(&Span::new(0, 10, 3)));

    assert_eq!(outer.to_range(), 10..20);
    assert_eq!(outer.to_string(), "10..20@3");
}

#[test]
fn empty_expand_is_done() {
    struct ExpandEmpty;
    impl Pass for ExpandEmpty {
        type Ctx = ();
        fn parse(&self, _source: &str, _span: Span, _ctx: &()) -> Outcome<()> {
            Outcome::Expand(Vec::new())
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(ExpandEmpty);
    let mut tree = root("abc");

    let report = run_rounds(schedule, "abc", &mut tree);

    assert!(report.reached_fixpoint);
    assert_eq!(tree.status(tree.root()), Status::Done);
    assert_eq!(tree.children(tree.root()).len(), 0);
}

#[test]
fn failed_node_is_retried_by_next_pass() {
    let mut schedule = Schedule::new();
    schedule.push(fail_pass());
    schedule.push(done_pass());

    let mut tree = root("abc");
    let report = run_rounds(schedule, "abc", &mut tree);

    assert!(report.reached_fixpoint);
    assert_eq!(report.rounds_run, 2);
    assert_eq!(report.nodes_processed, 2);
    assert_eq!(report.nodes_failed, 1);
    assert_eq!(tree.status(tree.root()), Status::Done);
    assert_eq!(tree.attempts(tree.root()), 1);
}

#[test]
fn retries_exhaust_after_schedule_ends() {
    let mut schedule = Schedule::new();
    schedule.push(fail_pass());
    schedule.push(fail_pass());

    let mut tree = root("abc");
    let report = run_rounds(schedule, "abc", &mut tree);

    assert_eq!(report.rounds_run, 2);
    assert_eq!(report.nodes_failed, 2);
    assert_eq!(tree.status(tree.root()), Status::Failed);
    assert_eq!(tree.attempts(tree.root()), 2);
    assert!(tree.pending(2).is_empty());
    assert!(report.reached_fixpoint);
}

#[test]
fn strict_shrink_is_enforced_by_default() {
    struct SameSpanChild;
    impl Pass for SameSpanChild {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            Outcome::Expand(vec![(span, ())])
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(SameSpanChild);

    let mut tree = root("abc");
    let report = run_rounds(schedule, "abc", &mut tree);

    assert_eq!(report.nodes_failed, 1);
    assert_eq!(tree.status(tree.root()), Status::Failed);
    assert!(tree.children(tree.root()).is_empty());
}

#[test]
fn shrink_requirement_can_be_relaxed() {
    struct SameSpanChild;
    impl Pass for SameSpanChild {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            Outcome::Expand(vec![(span, ())])
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(SameSpanChild);
    schedule.push(done_pass());

    let engine = Engine::with_config(
        schedule,
        EngineConfig {
            enforce_shrink: false,
            max_rounds: None,
        },
    );
    let mut tree = root("abc");
    let report = engine.run("abc", &mut tree, &SerialExecutor, &CancelToken::new());

    assert!(report.reached_fixpoint);
    assert_eq!(tree.status(tree.root()), Status::Expanded);
    assert_eq!(tree.children(tree.root()).len(), 1);
    assert_eq!(tree.status(tree.children(tree.root())[0]), Status::Done);
}

#[test]
fn out_of_parent_child_is_rejected() {
    struct EscapeParent;
    impl Pass for EscapeParent {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            Outcome::Expand(vec![(Span::new(span.start, span.end + 5, span.rev), ())])
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(EscapeParent);

    let mut tree = root("abc");
    let report = run_rounds(schedule, "abc", &mut tree);

    assert_eq!(report.nodes_failed, 1);
    assert_eq!(tree.status(tree.root()), Status::Failed);
}

#[test]
fn stale_revision_child_is_rejected() {
    struct WrongRevision;
    impl Pass for WrongRevision {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            let child = Span::new(span.start, span.end - 1, span.rev + 1);
            Outcome::Expand(vec![(child, ())])
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(WrongRevision);

    let mut tree = root("abcd");
    let report = run_rounds(schedule, "abcd", &mut tree);

    assert_eq!(report.nodes_failed, 1);
    assert_eq!(tree.status(tree.root()), Status::Failed);
}

#[test]
fn children_preserve_outcome_order() {
    struct SplitInThree;
    impl Pass for SplitInThree {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            let third = span.len() / 3;
            let rev = span.rev;
            Outcome::Expand(vec![
                (Span::new(span.start, span.start + third, rev), ()),
                (
                    Span::new(span.start + third, span.start + 2 * third, rev),
                    (),
                ),
                (Span::new(span.start + 2 * third, span.end, rev), ()),
            ])
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(SplitInThree);
    schedule.push(done_pass());

    let mut tree = root("abcdef");
    run_rounds(schedule, "abcdef", &mut tree);

    let kids = tree.children(tree.root());
    assert_eq!(kids.len(), 3);
    assert_eq!(tree.span(kids[0]).to_range(), 0..2);
    assert_eq!(tree.span(kids[1]).to_range(), 2..4);
    assert_eq!(tree.span(kids[2]).to_range(), 4..6);
    assert_eq!(tree.text("abcdef", kids[1]), "cd");
}

#[test]
fn second_run_after_fixpoint_is_a_no_op() {
    let mut schedule = Schedule::new();
    schedule.push(done_pass());

    let mut tree = root("abc");
    let first = run_rounds(schedule, "abc", &mut tree);
    assert!(first.reached_fixpoint);

    let schedule2 = Schedule::new();
    let second = run_rounds(schedule2, "abc", &mut tree);
    assert!(second.reached_fixpoint);
    assert_eq!(second.nodes_processed, 0);
    assert_eq!(second.rounds_run, 0);
}

#[test]
fn pre_cancelled_run_processes_nothing() {
    let mut schedule = Schedule::new();
    schedule.push(done_pass());

    let token = CancelToken::new();
    token.cancel();

    let engine = Engine::new(schedule);
    let mut tree = root("abc");
    let report = engine.run("abc", &mut tree, &SerialExecutor, &token);

    assert!(report.cancelled);
    assert!(!report.reached_fixpoint);
    assert_eq!(report.nodes_processed, 0);
    assert_eq!(tree.status(tree.root()), Status::Unparsed);
}

#[test]
fn mid_run_cancellation_stops_between_jobs() {
    struct CancelAfterFirst {
        token: CancelToken,
        seen: AtomicUsize,
    }
    impl Pass for CancelAfterFirst {
        type Ctx = ();
        fn parse(&self, _source: &str, _span: Span, _ctx: &()) -> Outcome<()> {
            if self.seen.fetch_add(1, Ordering::SeqCst) == 0 {
                self.token.cancel();
            }
            Outcome::Done
        }
    }

    struct SplitInThree;
    impl Pass for SplitInThree {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            let third = span.len() / 3;
            let rev = span.rev;
            Outcome::Expand(vec![
                (Span::new(span.start, span.start + third, rev), ()),
                (
                    Span::new(span.start + third, span.start + 2 * third, rev),
                    (),
                ),
                (Span::new(span.start + 2 * third, span.end, rev), ()),
            ])
        }
    }

    let token = CancelToken::new();
    let mut schedule = Schedule::new();
    schedule.push(SplitInThree);
    schedule.push(CancelAfterFirst {
        token: token.clone(),
        seen: AtomicUsize::new(0),
    });

    let engine = Engine::new(schedule);
    let mut tree = root("abcdef");
    let report = engine.run("abcdef", &mut tree, &SerialExecutor, &token);

    assert!(report.cancelled);
    assert_eq!(report.rounds_run, 2);
    assert_eq!(report.nodes_processed, 2);

    let done = tree
        .nodes()
        .filter(|id| tree.status(*id) == Status::Done)
        .count();
    assert_eq!(done, 1);
    assert_eq!(tree.pending(2).len(), 2);

    token.reset();
    let resumed = Engine::new(resume_schedule()).run("abcdef", &mut tree, &SerialExecutor, &token);
    assert!(resumed.reached_fixpoint);
}
fn resume_schedule() -> Schedule<()> {
    let mut schedule = Schedule::new();
    schedule.push(fail_pass());
    schedule.push(done_pass());
    schedule
}

#[test]
fn status_counts_and_settled_queries() {
    struct SplitInTwo;
    impl Pass for SplitInTwo {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            let half = span.len() / 2;
            let rev = span.rev;
            Outcome::Expand(vec![
                (Span::new(span.start, span.start + half, rev), ()),
                (Span::new(span.start + half, span.end, rev), ()),
            ])
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(SplitInTwo);
    schedule.push(fail_pass());
    schedule.push(fail_pass());

    let mut tree = root("abcd");
    run_rounds(schedule, "abcd", &mut tree);

    let counts = tree.status_counts();
    assert_eq!(counts.total(), 3);
    assert_eq!(counts.expanded, 1);
    assert_eq!(counts.failed, 2);
    assert_eq!(counts.done, 0);
    assert_eq!(counts.unparsed, 0);
    assert_eq!(counts.get(Status::Failed), 2);

    assert!(tree.is_settled(tree.root(), 3));
    assert!(tree.pending(3).is_empty());
}

#[test]
fn max_rounds_cap_limits_work() {
    let mut schedule = Schedule::new();
    schedule.push(fail_pass());
    schedule.push(fail_pass());
    schedule.push(fail_pass());

    let engine = Engine::with_config(
        schedule,
        EngineConfig {
            enforce_shrink: true,
            max_rounds: Some(2),
        },
    );
    let mut tree = root("abc");
    let report = engine.run("abc", &mut tree, &SerialExecutor, &CancelToken::new());

    assert_eq!(report.rounds_run, 2);
    assert_eq!(tree.attempts(tree.root()), 2);
    assert!(tree.pending(2).is_empty());
}

#[test]
fn empty_schedule_leaves_tree_untouched() {
    let mut tree = root("abc");
    let report = run_rounds(Schedule::new(), "abc", &mut tree);
    assert!(!report.reached_fixpoint);
    assert!(!report.cancelled);
    assert_eq!(report.nodes_processed, 0);
    assert_eq!(tree.status(tree.root()), Status::Unparsed);
    assert_eq!(tree.pending(0), vec![tree.root()]);
}

#[test]
fn executor_contract_returns_prefix_in_order() {
    struct TakeTwo;
    impl Executor for TakeTwo {
        fn execute<C, F>(
            &self,
            jobs: Vec<Job<C>>,
            run: F,
            _cancel: &CancelToken,
        ) -> Vec<(Job<C>, Outcome<C>)>
        where
            C: Send + 'static,
            F: Fn(&Job<C>) -> Outcome<C> + Send + Sync,
        {
            jobs.into_iter()
                .take(2)
                .map(|job| {
                    let outcome = run(&job);
                    (job, outcome)
                })
                .collect()
        }
    }

    struct SplitInThree;
    impl Pass for SplitInThree {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            let third = span.len() / 3;
            let rev = span.rev;
            Outcome::Expand(vec![
                (Span::new(span.start, span.start + third, rev), ()),
                (
                    Span::new(span.start + third, span.start + 2 * third, rev),
                    (),
                ),
                (Span::new(span.start + 2 * third, span.end, rev), ()),
            ])
        }
    }

    let mut schedule = Schedule::new();
    schedule.push(SplitInThree);
    schedule.push(done_pass());

    let engine = Engine::new(schedule);
    let mut tree = root("abcdef");
    let report = engine.run("abcdef", &mut tree, &TakeTwo, &CancelToken::new());

    assert!(report.cancelled);
    assert_eq!(report.rounds_run, 2);
    assert_eq!(report.nodes_processed, 3);

    let unparsed = tree
        .nodes()
        .filter(|id| tree.status(*id) == Status::Unparsed)
        .count();
    assert_eq!(unparsed, 1);
}

#[test]
fn tree_text_slices_source() {
    let tree: ParseTree<()> = root("hello world");
    assert_eq!(tree.text("hello world", tree.root()), "hello world");
    assert_eq!(tree.source_rev(), 0);
    assert_eq!(tree.root(), NodeId(0));
    assert_eq!(tree.parent(tree.root()), None);
    assert_eq!(tree.depth(tree.root()), 0);
    assert!(tree.is_root_only());
}

#[cfg(feature = "parallel")]
mod parallel {
    use super::*;
    use incraparse::RayonExecutor;

    struct SplitInThree;
    impl Pass for SplitInThree {
        type Ctx = ();
        fn parse(&self, _source: &str, span: Span, _ctx: &()) -> Outcome<()> {
            let third = span.len() / 3;
            let rev = span.rev;
            if span.len() <= 2 {
                return Outcome::Done;
            }
            Outcome::Expand(vec![
                (Span::new(span.start, span.start + third, rev), ()),
                (
                    Span::new(span.start + third, span.start + 2 * third, rev),
                    (),
                ),
                (Span::new(span.start + 2 * third, span.end, rev), ()),
            ])
        }
    }

    fn make_schedule() -> Schedule<()> {
        let mut schedule = Schedule::new();
        schedule.push(SplitInThree);
        schedule.push(SplitInThree);
        schedule.push(SplitInThree);
        schedule
    }

    #[test]
    fn rayon_and_serial_agree() {
        let mut serial_tree = root("abcdefghijkl");
        run_rounds(make_schedule(), "abcdefghijkl", &mut serial_tree);
        assert!(serial_tree.is_settled(serial_tree.root(), 3));

        let mut par_tree = root("abcdefghijkl");
        let engine = Engine::new(make_schedule());
        let report = engine.run(
            "abcdefghijkl",
            &mut par_tree,
            &RayonExecutor,
            &CancelToken::new(),
        );

        assert!(report.reached_fixpoint);

        let shape = |tree: &ParseTree<()>| -> Vec<(String, Status)> {
            tree.nodes()
                .map(|id| (format!("{:?}", tree.span(id)), tree.status(id)))
                .collect()
        };
        assert_eq!(shape(&serial_tree), shape(&par_tree));
        assert!(par_tree.is_settled(par_tree.root(), 3));
    }
}
