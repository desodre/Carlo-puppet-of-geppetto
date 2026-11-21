//! The walk against a scripted transport: no Android, no Binder, no real clock.
use super::*;
use crate::accessibility::{
    node::Properties,
    window::fixture::{Spec, reply},
};
use std::collections::VecDeque;

const SECRET: &str = "s3cr3t-sentinel";

type Answer = Result<Vec<Node>, Error>;

struct Fake {
    now: Instant,
    /// Clock advance per `find`.
    tick: Duration,
    /// Queue of window replies; the last one repeats.
    windows: VecDeque<Result<Vec<u8>, Error>>,
    /// `(window, node id)` to node; the world the "apps" answer from.
    nodes: HashMap<(i32, i64), Node>,
    roots: HashMap<i32, i64>,
    /// Scripted outcomes consumed by requests for a node, oldest first.
    script: HashMap<(i32, i64), VecDeque<Answer>>,
    /// Nodes whose presence in an answer makes it undecodable.
    poison: HashMap<(i32, i64), Error>,
    /// Nodes an answer holds at most, the requested one included.
    batch: usize,
    log: Vec<(i32, i64, i32)>,
    window_calls: u32,
    pauses: Vec<Duration>,
    invalidations: u32,
    /// Invalidations delivered while a request is answered.
    invalidate_on_find: u32,
}

impl Fake {
    fn new(windows: Vec<u8>) -> Self {
        Self {
            now: Instant::now(),
            tick: Duration::from_millis(1),
            windows: VecDeque::from([Ok(windows)]),
            nodes: HashMap::new(),
            roots: HashMap::new(),
            script: HashMap::new(),
            poison: HashMap::new(),
            batch: 51,
            log: Vec::new(),
            window_calls: 0,
            pauses: Vec::new(),
            invalidations: 0,
            invalidate_on_find: 0,
        }
    }
    /// One display 0 window per `(id, layer)`.
    fn with_windows(windows: &[(i32, i32)]) -> Self {
        let specs = windows
            .iter()
            .map(|(id, layer)| Spec::new(0, *id, *layer))
            .collect();
        Self::new(reply(&[(0, specs)]))
    }
    fn add(&mut self, window: i32, id: i64, children: &[i64]) -> &mut Node {
        let node = Node {
            source_id: id,
            window_id: window,
            children: children.to_vec(),
            class: Some("android.view.View".into()),
            ..Node::default()
        };
        self.nodes.insert((window, id), node);
        self.nodes.get_mut(&(window, id)).unwrap()
    }
    fn root(&mut self, window: i32, id: i64) {
        self.roots.insert(window, id);
    }
    fn script(&mut self, window: i32, id: i64, outcome: Result<Vec<Node>, Error>) {
        self.script
            .entry((window, id))
            .or_default()
            .push_back(outcome);
    }
    fn sequence(&self) -> Sequence {
        Sequence::new(self.now)
    }
    fn run(&mut self) -> (Stats, Result<String, Error>) {
        let mut sequence = self.sequence();
        self.run_in(&mut sequence)
    }
    fn run_in(&mut self, sequence: &mut Sequence) -> (Stats, Result<String, Error>) {
        let (stats, result) = snapshot(self, sequence);
        (stats, result.map(|bytes| String::from_utf8(bytes).unwrap()))
    }
}

impl Transport for Fake {
    fn now(&mut self) -> Instant {
        self.now
    }
    fn pause(&mut self, duration: Duration) {
        self.pauses.push(duration);
        self.now += duration;
    }
    fn windows(&mut self) -> Result<Vec<u8>, Error> {
        self.window_calls += 1;
        if self.windows.len() > 1 {
            self.windows.pop_front().unwrap()
        } else {
            self.windows[0].clone()
        }
    }
    fn find(
        &mut self,
        window: i32,
        node: i64,
        prefetch: i32,
        deadline: Instant,
    ) -> Result<Vec<Node>, Error> {
        self.log.push((window, node, prefetch));
        self.now += self.tick;
        self.invalidations += self.invalidate_on_find;
        if self.now >= deadline {
            return Err(Error::QueryTimeout);
        }
        let id = if node == contract::ROOT_NODE {
            match self.roots.get(&window) {
                Some(id) => *id,
                None => return Ok(Vec::new()),
            }
        } else {
            node
        };
        if let Some(outcome) = self
            .script
            .get_mut(&(window, id))
            .and_then(VecDeque::pop_front)
        {
            return outcome;
        }
        let Some(first) = self.nodes.get(&(window, id)) else {
            return Ok(Vec::new());
        };
        // Depth-first descendants, as the app's prefetcher orders them.
        let mut answer = vec![first.clone()];
        if prefetch != 0 {
            assert_eq!(prefetch, 8 | 32, "descendants depth first, uninterruptible");
            let mut stack: Vec<i64> = first.children.iter().rev().copied().collect();
            let mut listed = HashSet::from([id]);
            while let Some(next) = stack.pop() {
                if answer.len() >= self.batch {
                    break;
                }
                let Some(found) = self.nodes.get(&(window, next)) else {
                    continue;
                };
                if !listed.insert(next) {
                    continue;
                }
                answer.push(found.clone());
                stack.extend(found.children.iter().rev());
            }
        }
        for candidate in &answer {
            if let Some(error) = self.poison.get(&(window, candidate.source_id)) {
                return Err(error.clone());
            }
        }
        Ok(answer)
    }
    fn invalidations(&mut self) -> u32 {
        self.invalidations
    }
}

/// A chain of `length` nodes (ids 1..=length) as the tree of `window`.
fn chain(fake: &mut Fake, window: i32, length: i64) {
    for id in 1..=length {
        let children: Vec<i64> = if id < length { vec![id + 1] } else { vec![] };
        fake.add(window, id, &children);
    }
    fake.root(window, 1);
}

fn node_count(xml: &str) -> usize {
    xml.matches("<node ").count()
}

#[test]
fn happy_path_reads_several_windows_in_batches() {
    let mut fake = Fake::with_windows(&[(7, 1), (9, 5)]);
    // Window 9: a root with 12 rows of 10 cells (133 nodes). Window 7: 3 nodes.
    let rows: Vec<i64> = (1..=12).map(|row| row * 100).collect();
    fake.add(9, 1, &rows);
    for row in &rows {
        let cells: Vec<i64> = (1..=10).map(|cell| row + cell).collect();
        fake.add(9, *row, &cells);
        for cell in cells {
            fake.add(9, cell, &[]).text = Some(format!("cell {cell}"));
        }
    }
    fake.root(9, 1);
    fake.add(7, 50, &[51, 52]);
    fake.add(7, 51, &[]);
    fake.add(7, 52, &[]);
    fake.root(7, 50);
    let (stats, result) = fake.run();
    let xml = result.unwrap();
    assert_eq!(
        (stats.windows, stats.empty_windows, stats.nodes),
        (2, 0, 136)
    );
    assert_eq!(stats.max_depth, 3);
    assert_eq!(
        (stats.attempts, stats.duplicates, stats.invalidations),
        (1, 0, 0)
    );
    assert_eq!(
        stats.displays,
        [DisplayStats {
            display_id: 0,
            windows: 2,
            empty_windows: 0
        }]
    );
    // The root answer holds 51 nodes and ends inside row 500. An answer only
    // prefetches descendants of the node asked for, so the five cells it cut
    // off cost one request each; rows 600..1200 then arrive with their cells.
    assert_eq!(
        (stats.requests, stats.batches, stats.single_fallbacks),
        (14, 14, 0)
    );
    assert!(fake.log.iter().all(|(_, _, prefetch)| *prefetch == 40));
    assert_eq!(
        fake.log[0],
        (9, contract::ROOT_NODE, 40),
        "topmost layer first"
    );
    assert_eq!(
        fake.log[1..7],
        [506, 507, 508, 509, 510, 600].map(|id| (9, id, 40))
    );
    assert_eq!(fake.log[13], (7, contract::ROOT_NODE, 40));
    assert_eq!(node_count(&xml), 136);
    assert_eq!(stats.xml_bytes, Some(xml.len() as u64));
    // Document order: window 9 (layer 5) before window 7 (layer 1), cells in order.
    let first = xml.find("window-id=\"9\"").unwrap();
    let second = xml.find("window-id=\"7\"").unwrap();
    assert!(first < second);
    assert!(xml.find("cell 101\"").unwrap() < xml.find("cell 110\"").unwrap());
    assert!(xml.find("cell 110\"").unwrap() < xml.find("cell 201\"").unwrap());
    assert!(xml.contains("windows=\"2\"") && xml.contains("nodes=\"136\""));
    assert_eq!(fake.window_calls, 1);
    assert!(fake.pauses.is_empty());
}

#[test]
fn small_batches_request_exactly_the_missing_children() {
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, 10);
    fake.batch = 4;
    let (stats, result) = fake.run();
    assert_eq!(node_count(&result.unwrap()), 10);
    assert_eq!((stats.nodes, stats.max_depth), (10, 10));
    // Root + 3, then node 5 + 3, then node 9 + 1.
    assert_eq!(
        fake.log,
        [(3, contract::ROOT_NODE, 40), (3, 5, 40), (3, 9, 40)]
    );
    assert_eq!((stats.requests, stats.batches), (3, 3));
}

#[test]
fn a_node_listed_twice_is_emitted_once_and_counted() {
    let mut fake = Fake::with_windows(&[(3, 1)]);
    fake.add(3, 1, &[2, 3, 2]);
    fake.add(3, 2, &[4]);
    fake.add(3, 3, &[4]);
    fake.add(3, 4, &[]).text = Some("shared".into());
    fake.root(3, 1);
    let (stats, result) = fake.run();
    let xml = result.unwrap();
    assert_eq!((stats.nodes, stats.duplicates), (4, 2));
    assert_eq!(xml.matches("text=\"shared\"").count(), 1);
    assert_eq!(node_count(&xml), 4);
    // Node 3 keeps its position among the listed children.
    assert!(
        xml.contains("<node index=\"1\" text=\"\" resource-id=\"\" class=\"android.view.View\"")
    );
}

#[test]
fn a_cycle_is_a_typed_error_not_an_endless_walk() {
    for back_edge in [1, 2, 3] {
        let mut fake = Fake::with_windows(&[(3, 1)]);
        fake.add(3, 1, &[2]);
        fake.add(3, 2, &[3]);
        fake.add(3, 3, &[back_edge]);
        fake.root(3, 1);
        let (stats, result) = fake.run();
        assert_eq!(result.unwrap_err(), Error::InvalidTree("cycle"));
        assert_eq!((stats.attempts, stats.xml_bytes), (1, None));
    }
}

#[test]
fn depth_limit_is_a_typed_truncation_naming_the_limit() {
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, MAX_DEPTH as i64);
    let (stats, result) = fake.run();
    assert_eq!(node_count(&result.unwrap()), MAX_DEPTH);
    assert_eq!(stats.max_depth, MAX_DEPTH as u32);
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, MAX_DEPTH as i64 + 1);
    let (stats, result) = fake.run();
    assert_eq!(result.unwrap_err(), Error::Truncated { limit: "depth" });
    assert_eq!((stats.attempts, stats.xml_bytes), (1, None));
}

#[test]
fn node_limit_is_a_typed_truncation_across_windows() {
    let wide = |fake: &mut Fake, window: i32, leaves: i64| {
        let children: Vec<i64> = (2..leaves + 2).collect();
        fake.add(window, 1, &children);
        for child in children {
            fake.add(window, child, &[]);
        }
        fake.root(window, 1);
    };
    let mut fake = Fake::with_windows(&[(3, 2), (4, 1)]);
    wide(&mut fake, 3, 4_999);
    wide(&mut fake, 4, 4_999);
    let (stats, result) = fake.run();
    assert_eq!(node_count(&result.unwrap()), MAX_NODES);
    assert_eq!(stats.nodes, MAX_NODES as u32);
    let mut fake = Fake::with_windows(&[(3, 2), (4, 1)]);
    wide(&mut fake, 3, 4_999);
    wide(&mut fake, 4, 5_000);
    let (stats, result) = fake.run();
    assert_eq!(result.unwrap_err(), Error::Truncated { limit: "nodes" });
    assert_eq!(stats.xml_bytes, None);
}

#[test]
fn xml_byte_limit_is_a_typed_truncation_with_no_document() {
    let mut fake = Fake::with_windows(&[(3, 1)]);
    let children: Vec<i64> = (2..9_002).collect();
    fake.add(3, 1, &children);
    let long = "x".repeat(400);
    for child in children {
        fake.add(3, child, &[]).text = Some(long.clone());
    }
    fake.root(3, 1);
    let (stats, result) = fake.run();
    assert_eq!(result.unwrap_err(), Error::Truncated { limit: "xml_bytes" });
    assert_eq!(
        (stats.nodes, stats.xml_bytes, stats.generation),
        (9_001, None, 0)
    );
}

#[test]
fn a_missing_child_retries_the_whole_snapshot_then_is_inconsistent() {
    let build = || {
        let mut fake = Fake::with_windows(&[(3, 1)]);
        fake.add(3, 1, &[2, 3]);
        fake.add(3, 2, &[]);
        fake.root(3, 1); // node 3 is listed and does not exist
        fake
    };
    let mut fake = build();
    let (stats, result) = fake.run();
    assert_eq!(
        result.unwrap_err(),
        Error::InconsistentSnapshot {
            reason: "child_missing",
            attempts: 3
        }
    );
    assert_eq!((stats.attempts, stats.xml_bytes), (3, None));
    assert_eq!(
        fake.window_calls, 3,
        "every attempt starts from the window list"
    );
    assert_eq!(fake.pauses, [RETRY_PAUSE, RETRY_PAUSE]);
    assert_eq!(stats.requests, 6);
    // The same tree settling before the second attempt is a success: the first
    // answer lacks node 3 and the app then denies having it, once.
    let mut fake = build();
    let partial = vec![fake.nodes[&(3, 1)].clone(), fake.nodes[&(3, 2)].clone()];
    fake.script(3, 1, Ok(partial));
    fake.script(3, 3, Ok(Vec::new()));
    fake.add(3, 3, &[]);
    let (stats, result) = fake.run();
    assert_eq!(node_count(&result.unwrap()), 3);
    assert_eq!((stats.attempts, stats.nodes), (2, 3));
    // Nothing of the first attempt is reused: the root was asked for again.
    assert_eq!(
        fake.log
            .iter()
            .filter(|entry| entry.1 == contract::ROOT_NODE)
            .count(),
        2
    );
}

#[test]
fn a_window_that_disappears_or_times_out_makes_the_attempt_inconsistent() {
    for (error, reason) in [
        (Error::WindowGone, "window_gone"),
        (Error::QueryRejected, "window_gone"),
        (Error::QueryTimeout, "request_timeout"),
    ] {
        let mut fake = Fake::with_windows(&[(3, 2), (4, 1)]);
        chain(&mut fake, 3, 2);
        chain(&mut fake, 4, 2);
        for _ in 0..3 {
            fake.script(4, 1, Err(error.clone()));
        }
        let (stats, result) = fake.run();
        assert_eq!(
            result.unwrap_err(),
            Error::InconsistentSnapshot {
                reason,
                attempts: 3
            }
        );
        assert_eq!(stats.attempts, 3);
        // Gone once, present again: the retry succeeds.
        let mut fake = Fake::with_windows(&[(3, 2), (4, 1)]);
        chain(&mut fake, 3, 2);
        chain(&mut fake, 4, 2);
        fake.script(4, 1, Err(error.clone()));
        let (stats, result) = fake.run();
        assert_eq!(node_count(&result.unwrap()), 4);
        assert_eq!(stats.attempts, 2);
    }
    // Other transport failures are not retried.
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, 2);
    fake.script(3, 1, Err(Error::Cancelled));
    let (stats, result) = fake.run();
    assert_eq!(result.unwrap_err(), Error::Cancelled);
    assert_eq!(stats.attempts, 1);
}

#[test]
fn empty_window_lists_and_null_roots_are_reported_not_errors() {
    let mut fake = Fake::new(reply(&[]));
    let (stats, result) = fake.run();
    let xml = result.unwrap();
    assert_eq!((stats.windows, stats.nodes, stats.requests), (0, 0, 0));
    assert!(stats.displays.is_empty());
    assert!(
        xml.ends_with("windows=\"0\" empty-windows=\"0\" nodes=\"0\" displays=\"\"></hierarchy>")
    );
    // A display with no windows, and a window whose root is null.
    let mut fake = Fake::new(reply(&[
        (0, vec![Spec::new(0, 3, 2), Spec::new(0, 4, 1)]),
        (5, vec![]),
    ]));
    chain(&mut fake, 4, 1);
    let (stats, result) = fake.run();
    let xml = result.unwrap();
    assert_eq!((stats.windows, stats.empty_windows, stats.nodes), (2, 1, 1));
    assert_eq!(
        stats.displays,
        [
            DisplayStats {
                display_id: 0,
                windows: 2,
                empty_windows: 1
            },
            DisplayStats {
                display_id: 5,
                windows: 0,
                empty_windows: 0
            },
        ]
    );
    assert!(xml.contains(
        "windows=\"1\" empty-windows=\"1\" nodes=\"1\" displays=\"0:1080x2340 5:unknown\""
    ));
    assert!(!xml.contains("window-id=\"3\"") && xml.contains("window-id=\"4\""));
}

#[test]
fn null_or_undecodable_window_lists_are_typed_and_name_the_field() {
    let mut fake = Fake::new(vec![0, 0, 0, 0]);
    let (stats, result) = fake.run();
    assert_eq!(result.unwrap_err(), Error::WindowsUnavailable);
    assert_eq!((stats.attempts, stats.requests), (1, 0));
    let mut bad = reply(&[(0, vec![Spec::new(0, 3, 1)])]);
    let region = 12 + 108 + 28;
    bad[region..region + 4].copy_from_slice(&6_i32.to_le_bytes());
    let mut fake = Fake::new(bad);
    let (_, result) = fake.run();
    assert_eq!(
        result.unwrap_err(),
        Error::InvalidWindowList {
            field: "region",
            reason: "count_not_multiple_of_four"
        }
    );
    let mut fake = Fake::new(Vec::new());
    fake.windows = VecDeque::from([Err(Error::Binder {
        operation: "get_windows",
        status: -32,
    })]);
    let (_, result) = fake.run();
    assert_eq!(
        result.unwrap_err(),
        Error::Binder {
            operation: "get_windows",
            status: -32
        }
    );
}

#[test]
fn the_total_deadline_is_a_typed_truncation() {
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, 40);
    fake.batch = 1;
    fake.tick = Duration::from_millis(400);
    let (stats, result) = fake.run();
    assert_eq!(result.unwrap_err(), Error::Truncated { limit: "deadline" });
    assert_eq!((stats.attempts, stats.xml_bytes), (1, None));
    assert!(stats.requests <= 26 && stats.elapsed_ms >= 10_000);
    // Each request gets the shorter of its own bound and what is left in total.
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, 2);
    fake.tick = REQUEST_DEADLINE;
    let (stats, result) = fake.run();
    // Three attempts of 2 s each time out as inconsistent, inside the 10 s.
    assert_eq!(
        result.unwrap_err(),
        Error::InconsistentSnapshot {
            reason: "request_timeout",
            attempts: 3
        }
    );
    assert_eq!(stats.requests, 3);
    // A window list that returns after the deadline is a truncation too.
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, 2);
    fake.tick = TOTAL_DEADLINE;
    let (_, result) = fake.run();
    assert_eq!(result.unwrap_err(), Error::Truncated { limit: "deadline" });
}

#[test]
fn undecodable_nodes_fail_the_snapshot_and_are_counted_per_class() {
    let span = Error::UnsupportedNodeField {
        field: "text",
        reason: "char_sequence_span",
    };
    let leash = Error::UnsupportedNodeField {
        field: "leashed_child",
        reason: "layout_not_pinned",
    };
    let broken = Error::InvalidNodeField {
        field: "range_info",
        reason: "range_type",
    };
    let mut fake = Fake::with_windows(&[(3, 2), (4, 1)]);
    fake.add(3, 1, &[2, 3, 4, 5]);
    fake.add(3, 2, &[]);
    fake.add(3, 3, &[30]);
    fake.add(3, 30, &[]);
    fake.add(3, 4, &[]);
    fake.add(3, 5, &[]);
    fake.root(3, 1);
    fake.poison.insert((3, 2), span.clone());
    fake.poison.insert((3, 4), span.clone());
    fake.poison.insert((3, 5), leash);
    chain(&mut fake, 4, 2);
    fake.poison.insert((4, 1), broken);
    let (stats, result) = fake.run();
    assert_eq!(
        result.unwrap_err(),
        Error::UnsupportedNode {
            class: "range_type",
            count: 4
        }
    );
    assert_eq!(
        stats.node_failures,
        [
            NodeFailure {
                kind: "invalid",
                field: "range_info",
                reason: "range_type",
                count: 1
            },
            NodeFailure {
                kind: "unsupported",
                field: "leashed_child",
                reason: "layout_not_pinned",
                count: 1
            },
            NodeFailure {
                kind: "unsupported",
                field: "text",
                reason: "char_sequence_span",
                count: 2
            },
        ]
    );
    // Not retried, no document; the decodable nodes were still walked and counted.
    assert_eq!((stats.attempts, stats.xml_bytes), (1, None));
    assert_eq!(stats.nodes, 3, "nodes 1, 3 and 30 of window 3");
    assert_eq!(
        stats.empty_windows, 0,
        "an undecodable root is not an empty window"
    );
    assert!(stats.single_fallbacks >= 4);
    // Every failing answer was followed by a request for that node alone.
    assert!(fake.log.contains(&(3, 2, 0)) && fake.log.contains(&(4, contract::ROOT_NODE, 0)));
    // An undecodable neighbour does not lose a decodable node.
    assert!(fake.log.contains(&(3, 3, 40)) || fake.log.contains(&(3, 3, 0)));
}

#[test]
fn nodes_of_another_window_are_dropped_and_a_foreign_root_is_an_error() {
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, 2);
    let mut stranger = Node {
        source_id: 99,
        window_id: 8,
        ..Node::default()
    };
    stranger.text = Some("other window".into());
    let answer = vec![
        fake.nodes[&(3, 1)].clone(),
        stranger.clone(),
        fake.nodes[&(3, 2)].clone(),
    ];
    fake.script(3, 1, Ok(answer));
    let (stats, result) = fake.run();
    let xml = result.unwrap();
    assert_eq!((stats.nodes, stats.foreign_nodes), (2, 1));
    assert!(!xml.contains("other window"));
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, 2);
    fake.script(3, 1, Ok(vec![stranger]));
    let (_, result) = fake.run();
    assert_eq!(
        result.unwrap_err(),
        Error::InvalidNodeField {
            field: "window_id",
            reason: "request_window_mismatch"
        }
    );
}

#[test]
fn invalidations_are_counted_and_nothing_is_cached_between_snapshots() {
    let mut fake = Fake::with_windows(&[(3, 1)]);
    chain(&mut fake, 3, 3);
    fake.invalidations = 5; // delivered before this snapshot: not counted
    fake.invalidate_on_find = 2;
    let mut sequence = fake.sequence();
    let (first, result) = fake.run_in(&mut sequence);
    assert!(result.unwrap().contains("snapshot-id=\"1\""));
    assert_eq!(
        (first.snapshot_id, first.invalidations, first.requests),
        (1, 2, 1)
    );
    // The screen changes; the second snapshot asks again and sees only the new tree.
    fake.nodes.get_mut(&(3, 3)).unwrap().text = Some("changed".into());
    fake.invalidate_on_find = 0;
    let (second, result) = fake.run_in(&mut sequence);
    let xml = result.unwrap();
    assert!(xml.contains("text=\"changed\"") && xml.contains("snapshot-id=\"2\""));
    assert_eq!(
        (second.snapshot_id, second.invalidations, second.requests),
        (2, 0, 1)
    );
    assert_eq!(fake.window_calls, 2);
    assert_eq!(fake.log.len(), 2);
}

#[test]
fn metadata_ids_times_and_geometry_generation() {
    let windows = |width: i32, height: i32| {
        let mut status = Spec::new(0, 3, 2);
        status.rects = vec![[0, 0, width, 80]];
        let mut app = Spec::new(0, 4, 1);
        app.rects = vec![[0, 80, width, height]];
        reply(&[(0, vec![status, app])])
    };
    let mut fake = Fake::new(windows(1080, 2340));
    chain(&mut fake, 4, 1);
    let mut sequence = fake.sequence();
    fake.now += Duration::from_millis(250);
    let (first, result) = fake.run_in(&mut sequence);
    let xml = result.unwrap();
    assert_eq!(
        (first.snapshot_id, first.generation, first.rotation),
        (1, 1, None)
    );
    assert_eq!(
        (first.start_ms, first.end_ms, first.elapsed_ms),
        (250, 252, 2)
    );
    assert!(xml.contains(
        "<hierarchy rotation=\"unknown\" snapshot-id=\"1\" start-ms=\"250\" end-ms=\"252\" \
         generation=\"1\" windows=\"1\" empty-windows=\"1\" nodes=\"1\" displays=\"0:1080x2340\">"
    ));
    // Same derived size: same generation. A failed snapshot consumes an id only.
    let (second, _) = fake.run_in(&mut sequence);
    assert_eq!((second.snapshot_id, second.generation), (2, 1));
    fake.windows = VecDeque::from([Ok(vec![0, 0, 0, 0])]);
    let (failed, result) = fake.run_in(&mut sequence);
    assert!(result.is_err());
    assert_eq!((failed.snapshot_id, failed.generation), (3, 0));
    // Rotated: the derived size differs, so the generation advances.
    fake.windows = VecDeque::from([Ok(windows(2340, 1080))]);
    let (rotated, result) = fake.run_in(&mut sequence);
    assert!(result.unwrap().contains("generation=\"2\""));
    assert_eq!((rotated.snapshot_id, rotated.generation), (4, 2));
    assert!(rotated.start_ms >= failed.end_ms);
}

/// The document `tests/fixtures/hierarchy_golden.xml` holds; a Python test
/// parses the same file with a real XML parser.
fn golden() -> (Stats, String) {
    let mut ime = Spec::new(0, 12, 9);
    ime.kind = 2;
    ime.rects = vec![[0, 1500, 1080, 2340]];
    let mut external = Spec::new(2, 30, 1);
    external.rects = vec![[0, 0, 1920, 1080]];
    let mut fake = Fake::new(reply(&[
        (2, vec![external]),
        (0, vec![Spec::new(0, 11, 4), ime]),
    ]));
    let root = fake.add(11, 1, &[2, 3, 4]);
    root.class = Some("android.widget.FrameLayout".into());
    root.package = Some("org.geppetto.carlo.fixture".into());
    root.bounds_in_screen = Bounds {
        left: 0,
        top: 0,
        right: 1080,
        bottom: 2340,
    };
    root.properties.enabled = true;
    let button = fake.add(11, 2, &[]);
    button.class = Some("android.widget.Button".into());
    button.package = Some("org.geppetto.carlo.fixture".into());
    button.text = Some("Ação <&> \"quoted\" 'single'".into());
    button.resource_id = Some("org.geppetto.carlo.fixture:id/botão".into());
    button.content_description = Some("line one\nline two\ttab\rreturn".into());
    button.bounds_in_screen = Bounds {
        left: 40,
        top: 200,
        right: 520,
        bottom: 320,
    };
    button.properties = Properties {
        checkable: true,
        checked: true,
        focusable: true,
        focused: true,
        selected: true,
        clickable: true,
        long_clickable: true,
        enabled: true,
        password: true,
        scrollable: true,
        visible_to_user: true,
        editable: false,
    };
    let list = fake.add(11, 3, &[5, 6]);
    list.class = Some("androidx.recyclerview.widget.RecyclerView".into());
    list.bounds_in_screen = Bounds {
        left: -20,
        top: 320,
        right: 1080,
        bottom: 2000,
    };
    list.properties.scrollable = true;
    fake.add(11, 5, &[]).text = Some("emoji 🪵 👩‍👩‍👧‍👦 終 \u{10ffff}".into());
    fake.add(11, 6, &[]).text = Some("nul\0 bell\u{7} noncharacter\u{ffff}".into());
    let structural = fake.add(11, 4, &[]);
    structural.class = None;
    fake.root(11, 1);
    let key = fake.add(12, 70, &[]);
    key.class = Some("android.inputmethodservice.Keyboard$Key".into());
    key.text = Some("q".into());
    key.properties.clickable = true;
    fake.root(12, 70);
    fake.add(30, 90, &[]).class = Some("android.view.View".into());
    fake.root(30, 90);
    let (stats, result) = fake.run();
    (stats, result.unwrap())
}

#[test]
fn golden_document_attribute_order_escaping_and_window_order() {
    let (stats, xml) = golden();
    let expected = include_str!("../../../tests/fixtures/hierarchy_golden.xml");
    assert!(
        xml == expected,
        "golden document changed; update the fixture deliberately"
    );
    assert_eq!(stats.sanitized_characters, 3);
    assert_eq!((stats.windows, stats.nodes, stats.max_depth), (3, 8, 3));
    assert!(xml.starts_with(
        "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation=\"unknown\""
    ));
    assert!(xml.ends_with("</node></hierarchy>") || xml.ends_with(" /></hierarchy>"));
    // The 17 attributes, in the documented order, on a fully populated node.
    assert!(xml.contains(concat!(
        "<node index=\"0\" text=\"Ação &lt;&amp;&gt; &quot;quoted&quot; &apos;single&apos;\" ",
        "resource-id=\"org.geppetto.carlo.fixture:id/botão\" class=\"android.widget.Button\" ",
        "package=\"org.geppetto.carlo.fixture\" ",
        "content-desc=\"line one&#10;line two&#9;tab&#13;return\" ",
        "checkable=\"true\" checked=\"true\" clickable=\"true\" enabled=\"true\" ",
        "focusable=\"true\" focused=\"true\" scrollable=\"true\" long-clickable=\"true\" ",
        "password=\"true\" selected=\"true\" bounds=\"[40,200][520,320]\" />"
    )));
    // A structural node without text or class is kept, with empty strings.
    assert!(xml.contains(concat!(
        "<node index=\"2\" text=\"\" resource-id=\"\" class=\"\" package=\"\" content-desc=\"\" ",
        "checkable=\"false\" checked=\"false\" clickable=\"false\" enabled=\"false\" ",
        "focusable=\"false\" focused=\"false\" scrollable=\"false\" long-clickable=\"false\" ",
        "password=\"false\" selected=\"false\" bounds=\"[0,0][0,0]\" />"
    )));
    assert!(xml.contains("text=\"emoji 🪵 👩‍👩‍👧‍👦 終 \u{10ffff}\""));
    assert!(xml.contains("text=\"nul\u{fffd} bell\u{fffd} noncharacter\u{fffd}\""));
    assert!(xml.contains("bounds=\"[-20,320][1080,2000]\""));
    // Display 0 before display 2; inside display 0 the keyboard (layer 9) first.
    let order: Vec<usize> = ["window-id=\"12\"", "window-id=\"11\"", "window-id=\"30\""]
        .iter()
        .map(|needle| xml.find(needle).unwrap())
        .collect();
    assert!(order[0] < order[1] && order[1] < order[2]);
    assert!(
        xml.contains("window-id=\"12\" display-id=\"0\" window-type=\"2\" window-layer=\"9\" />")
    );
    assert!(xml.contains("displays=\"0:1080x2340 2:1920x1080\""));
}

#[test]
fn probe_report_and_debug_output_carry_no_ui_contents() {
    let mut window = Spec::new(0, 3, 1);
    window.title = Some(SECRET);
    window.rects = vec![[0, 0, 7777, 8888]];
    let mut fake = Fake::new(reply(&[(0, vec![window])]));
    fake.add(3, 1, &[2, 3]).text = Some(SECRET.into());
    let child = fake.add(3, 2, &[]);
    child.content_description = Some(SECRET.into());
    child.resource_id = Some(format!("{SECRET}:id/x"));
    child.package = Some(SECRET.into());
    child.class = Some(SECRET.into());
    child.unique_id = Some(SECRET.into());
    child.bounds_in_screen = Bounds {
        left: 7001,
        top: 7002,
        right: 7003,
        bottom: 7004,
    };
    fake.add(3, 3, &[]);
    fake.root(3, 1);
    let environment =
        || Diagnostics::from_environment("android", "aarch64", Some(36), None, Some(2000), None);
    let session = || SessionOutcome {
        registration_flags: Some(contract::registration_flags()),
        initialized: true,
        connection_id: Some(4),
        unregistered: true,
        ..SessionOutcome::default()
    };
    let profile = Some(contract::Profile::SamsungA16xApi36);
    let mut sequence = fake.sequence();
    let (stats, result) = snapshot(&mut fake, &mut sequence);
    let document = result.unwrap();
    assert!(
        String::from_utf8(document.clone())
            .unwrap()
            .contains(SECRET)
    );
    let mut report = Report::build(
        environment(),
        None,
        profile,
        session(),
        Some((stats, Ok(document))),
    );
    assert!(report.passed());
    let json = serde_json::to_string_pretty(&report).unwrap();
    for hidden in [
        SECRET,
        "7777",
        "8888",
        "7001",
        "7004",
        "android.view",
        "title",
        "<node",
    ] {
        assert!(!json.contains(hidden), "report leaked {hidden}");
        assert!(
            !format!("{report:?}").contains(hidden),
            "debug leaked {hidden}"
        );
    }
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["profile"], "samsung_a16x_api36");
    assert_eq!(value["registration_flags"], 1);
    assert_eq!(value["snapshot"]["nodes"], 3);
    assert_eq!(value["snapshot"]["rotation"], serde_json::Value::Null);
    assert_eq!(value["snapshot"]["displays"][0]["windows"], 1);
    assert_eq!(value["limits"]["max_nodes"], 10_000);
    assert_eq!(value["limits"]["max_xml_bytes"], 4 * 1024 * 1024);
    assert_eq!(value["error"], serde_json::Value::Null);
    assert!(value.get("document").is_none() && value.get("xml").is_none());
    assert!(
        report
            .take_document()
            .is_some_and(|bytes| !bytes.is_empty())
    );
    assert!(report.take_document().is_none());
    // A failure report names the class and field, and still nothing else.
    fake.poison.insert(
        (3, 2),
        Error::UnsupportedNodeField {
            field: "text",
            reason: "char_sequence_span",
        },
    );
    let mut sequence = fake.sequence();
    let (stats, result) = snapshot(&mut fake, &mut sequence);
    let mut failed = Report::build(
        environment(),
        None,
        profile,
        session(),
        Some((stats, result)),
    );
    assert!(!failed.passed() && failed.take_document().is_none());
    let json = serde_json::to_string(&failed).unwrap();
    assert!(!json.contains(SECRET));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["error"]["kind"], "unsupported_node");
    assert_eq!(value["error"]["detail"]["class"], "char_sequence_span");
    assert_eq!(value["error"]["detail"]["count"], 1);
    assert_eq!(value["snapshot"]["node_failures"][0]["field"], "text");
    assert_eq!(value["snapshot"]["xml_bytes"], serde_json::Value::Null);
}

#[test]
fn a_document_is_withheld_unless_the_whole_session_was_clean() {
    let environment = || Diagnostics::from_environment("linux", "x86_64", None, None, None, None);
    let clean = || SessionOutcome {
        registration_flags: Some(1),
        initialized: true,
        connection_id: Some(1),
        unregistered: true,
        ..SessionOutcome::default()
    };
    let good = || {
        let mut fake = Fake::with_windows(&[(3, 1)]);
        chain(&mut fake, 3, 2);
        let mut sequence = fake.sequence();
        Some(snapshot(&mut fake, &mut sequence))
    };
    let profile = Some(contract::Profile::SamsungA16xApi36);
    let mut report = Report::build(environment(), None, profile, clean(), good());
    assert!(report.passed() && report.take_document().is_some());
    for spoil in 0..4 {
        let mut session = clean();
        match spoil {
            0 => session.unregistered = false,
            1 => {
                session.cleanup_error = Some(Error::Binder {
                    operation: "unregister",
                    status: -32,
                })
            }
            2 => session.error = Some(Error::InvalidCallback),
            _ => session.initialized = false,
        }
        let mut report = Report::build(environment(), None, profile, session, good());
        assert!(
            !report.passed() && report.take_document().is_none(),
            "case {spoil}"
        );
    }
    // Typed errors serialize with the names the documentation uses.
    for (error, text) in [
        (
            Error::UnsupportedProfile,
            r#"{"kind":"unsupported_profile"}"#,
        ),
        (
            Error::InconsistentSnapshot {
                reason: "child_missing",
                attempts: 3,
            },
            r#"{"kind":"inconsistent_snapshot","detail":{"reason":"child_missing","attempts":3}}"#,
        ),
        (
            Error::Truncated { limit: "nodes" },
            r#"{"kind":"truncated","detail":{"limit":"nodes"}}"#,
        ),
        (
            Error::UnsupportedNode {
                class: "char_sequence_span",
                count: 2,
            },
            r#"{"kind":"unsupported_node","detail":{"class":"char_sequence_span","count":2}}"#,
        ),
        (
            Error::InvalidWindowList {
                field: "region",
                reason: "truncated",
            },
            r#"{"kind":"invalid_window_list","detail":{"field":"region","reason":"truncated"}}"#,
        ),
        (
            Error::UnsupportedNodeField {
                field: "text",
                reason: "char_sequence_span",
            },
            r#"{"kind":"unsupported_node_field","detail":{"field":"text","reason":"char_sequence_span"}}"#,
        ),
        (
            Error::WindowsUnavailable,
            r#"{"kind":"windows_unavailable"}"#,
        ),
    ] {
        assert_eq!(serde_json::to_string(&error).unwrap(), text);
    }
}

#[test]
fn host_collect_refuses_without_a_session_or_a_document() {
    let mut report = collect().unwrap();
    assert!(!report.passed());
    assert_eq!(report.error, Some(Error::UnsupportedProfile));
    assert_eq!((report.profile, report.registration_flags), (None, None));
    assert!(report.snapshot.is_none() && report.take_document().is_none());
}
