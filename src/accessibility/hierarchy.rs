//! Complete hierarchy snapshot (docs/hierarchy.md): enumerate windows, walk every
//! tree through a [`Transport`], then serialize. Safe Rust; the Android session
//! is one implementation of the transport and tests use another.
//!
//! Nothing is cached between snapshots and nothing is atomic: a tree that
//! changes while it is read makes the attempt inconsistent, the whole snapshot is
//! retried a bounded number of times, and a partial tree is never a success.
//! Reports carry counters only; the XML is the single place UI contents appear.

use super::{
    Error, contract,
    node::{Bounds, Node},
    state,
    window::{self, Window},
    xml,
};
use crate::{diagnostics::Diagnostics, error::CarloError};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

/// Whole snapshot, every attempt included.
pub(super) const TOTAL_DEADLINE: Duration = Duration::from_secs(10);
/// One node request: an app answers from its UI thread in milliseconds; two
/// seconds tolerate a busy one and still leave room for the retries.
pub(super) const REQUEST_DEADLINE: Duration = Duration::from_secs(2);
pub(super) const MAX_NODES: usize = 10_000;
pub(super) const MAX_DEPTH: usize = 128;
pub(super) const MAX_XML_BYTES: usize = 4 * 1024 * 1024;
/// The first attempt plus two retries.
pub(super) const MAX_ATTEMPTS: u32 = 3;
/// Pause before a retry, so a window that is being replaced can settle.
const RETRY_PAUSE: Duration = Duration::from_millis(100);
/// Window tracking only starts when the session registers, so the first list
/// can be empty for a moment (observed on the executed build). An empty list is
/// polled for this long before it is reported as a genuinely empty screen.
const WINDOWS_SETTLE: Duration = Duration::from_secs(2);
const WINDOWS_POLL: Duration = Duration::from_millis(50);
/// One answer carries the requested node and up to 50 descendants in the order
/// the walk needs them. Without UNINTERRUPTIBLE the app would split the answer.
const PREFETCH: i32 =
    contract::PREFETCH_DESCENDANTS_DEPTH_FIRST | contract::PREFETCH_UNINTERRUPTIBLE;

/// What a snapshot needs from a registered session. Every call is bounded by
/// the implementation; `find` returns the nodes of one answer in wire order.
pub(super) trait Transport {
    fn now(&mut self) -> Instant;
    /// Wait without holding any lock; may return early when cancelled.
    fn pause(&mut self, duration: Duration);
    /// `getWindows` reply after its exception header.
    fn windows(&mut self) -> Result<Vec<u8>, Error>;
    /// An empty list is the app's "no such node".
    fn find(
        &mut self,
        window: i32,
        node: i64,
        prefetch: i32,
        deadline: Instant,
    ) -> Result<Vec<Node>, Error>;
    /// `onInterrupt`/`clearAccessibilityCache` deliveries so far in this session.
    fn invalidations(&mut self) -> u32;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct DisplayStats {
    display_id: i32,
    windows: u32,
    /// Windows whose root was null: legitimate, reported, not an error.
    empty_windows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NodeFailure {
    /// `unsupported` (encoding not pinned) or `invalid` (malformed).
    kind: &'static str,
    field: &'static str,
    reason: &'static str,
    count: u32,
}

/// Counters and metadata of one snapshot. No UI contents, names or geometry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Stats {
    /// Monotonic per process, starting at 1; failed snapshots consume an id.
    snapshot_id: u64,
    /// Milliseconds on the monotonic clock since this process first used it.
    start_ms: u64,
    end_ms: u64,
    elapsed_ms: u64,
    /// Changes when the derived display sizes differ from the previous
    /// successful snapshot of this process; 0 until a snapshot succeeds.
    generation: u64,
    /// Not available without another pinned contract: always null.
    rotation: Option<u32>,
    attempts: u32,
    requests: u32,
    batches: u32,
    /// Requests repeated without prefetch after an answer failed to decode.
    single_fallbacks: u32,
    /// Extra `getWindows` calls made while the first list was still empty.
    window_polls: u32,
    invalidations: u32,
    displays: Vec<DisplayStats>,
    windows: u32,
    empty_windows: u32,
    nodes: u32,
    max_depth: u32,
    duplicates: u32,
    /// Prefetched nodes that named another window and were dropped.
    foreign_nodes: u32,
    node_failures: Vec<NodeFailure>,
    /// Size of the document a dump would emit; null unless the snapshot succeeded.
    xml_bytes: Option<u64>,
    sanitized_characters: u64,
}

/// Process-wide snapshot numbering and geometry generation.
pub(super) struct Sequence {
    epoch: Instant,
    last_id: u64,
    generation: u64,
    sizes: Option<Vec<DisplaySize>>,
}

impl Sequence {
    pub(super) fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            last_id: 0,
            generation: 0,
            sizes: None,
        }
    }
}

/// Display id and the extent of the union of its window regions, when any.
type DisplaySize = (i32, Option<(i64, i64)>);

struct Element {
    node: Node,
    /// Position in the parent's child list (0 for a root).
    index: usize,
    children: Vec<usize>,
}

struct WindowTree {
    window: Window,
    root: Option<usize>,
}

#[derive(Default)]
struct Tree {
    arena: Vec<Element>,
    windows: Vec<WindowTree>,
    sizes: Vec<DisplaySize>,
}

enum Stop {
    /// The tree changed under the walk; the whole snapshot may be retried.
    Inconsistent(&'static str),
    Fatal(Error),
}
impl From<Error> for Stop {
    fn from(error: Error) -> Self {
        Self::Fatal(error)
    }
}

enum Fetched {
    Node(Box<Node>),
    Null,
    /// The node exists but cannot be decoded; recorded, never emitted.
    Failed,
}

/// Per-window walk state. Node ids are only unique inside one window.
struct Scope {
    window: i32,
    /// Prefetched nodes not yet placed in the tree.
    cache: HashMap<i64, Node>,
    /// Ids already placed in the tree or recorded as failed.
    seen: HashSet<i64>,
}

type Failures = BTreeMap<(&'static str, &'static str, &'static str), u32>;

struct Walk<'a> {
    transport: &'a mut dyn Transport,
    deadline: Instant,
    stats: &'a mut Stats,
}

impl Walk<'_> {
    fn check_deadline(&mut self) -> Result<(), Error> {
        if self.transport.now() >= self.deadline {
            Err(Error::Truncated { limit: "deadline" })
        } else {
            Ok(())
        }
    }

    fn attempts(&mut self) -> Result<Tree, Error> {
        let mut reason = "none";
        for attempt in 1..=MAX_ATTEMPTS {
            self.stats.attempts = attempt;
            if attempt > 1 {
                let now = self.transport.now();
                self.transport
                    .pause(RETRY_PAUSE.min(self.deadline.saturating_duration_since(now)));
            }
            self.check_deadline()?;
            match self.attempt() {
                Ok(tree) => return Ok(tree),
                Err(Stop::Inconsistent(why)) => reason = why,
                Err(Stop::Fatal(error)) => return Err(error),
            }
        }
        // The deadline outranks the inconsistency when both ended the last attempt.
        self.check_deadline()?;
        Err(Error::InconsistentSnapshot {
            reason,
            attempts: MAX_ATTEMPTS,
        })
    }

    /// One complete pass. Starts from nothing: no node or window survives a pass.
    fn attempt(&mut self) -> Result<Tree, Stop> {
        let stats = &mut *self.stats;
        stats.displays.clear();
        stats.node_failures.clear();
        (stats.windows, stats.empty_windows, stats.nodes) = (0, 0, 0);
        (stats.max_depth, stats.duplicates, stats.foreign_nodes) = (0, 0, 0);
        let settle = self.transport.now() + WINDOWS_SETTLE;
        let mut displays = loop {
            let reply = self.transport.windows()?;
            self.check_deadline()?;
            let displays = window::decode(&reply)?.ok_or(Error::WindowsUnavailable)?;
            let now = self.transport.now();
            if displays.iter().any(|display| !display.windows.is_empty()) || now >= settle {
                break displays;
            }
            self.stats.window_polls += 1;
            self.transport
                .pause(WINDOWS_POLL.min(settle.saturating_duration_since(now)));
        };
        // Document order: displays by ascending id, windows topmost first.
        displays.sort_by_key(|display| display.id);
        let mut tree = Tree::default();
        let mut failures = Failures::new();
        for mut display in displays {
            display
                .windows
                .sort_by(|a, b| b.layer.cmp(&a.layer).then(a.id.cmp(&b.id)));
            let mut counted = DisplayStats {
                display_id: display.id,
                ..DisplayStats::default()
            };
            let mut union: Option<Bounds> = None;
            for window in display.windows {
                if let Some(bounds) = window.bounds {
                    union = Some(union.map_or(bounds, |old| Bounds {
                        left: old.left.min(bounds.left),
                        top: old.top.min(bounds.top),
                        right: old.right.max(bounds.right),
                        bottom: old.bottom.max(bounds.bottom),
                    }));
                }
                let (root, failed) = self.window(&window, &mut tree, &mut failures)?;
                counted.windows += 1;
                if root.is_none() && !failed {
                    counted.empty_windows += 1;
                }
                tree.windows.push(WindowTree { window, root });
            }
            self.stats.windows += counted.windows;
            self.stats.empty_windows += counted.empty_windows;
            self.stats.displays.push(counted);
            tree.sizes.push((
                display.id,
                union.map(|b| {
                    (
                        i64::from(b.right) - i64::from(b.left),
                        i64::from(b.bottom) - i64::from(b.top),
                    )
                }),
            ));
        }
        if let Some(((_, _, class), _)) = failures.iter().next() {
            let class = *class;
            let count = failures
                .values()
                .fold(0_u32, |sum, n| sum.saturating_add(*n));
            self.stats.node_failures = failures
                .iter()
                .map(|((kind, field, reason), count)| NodeFailure {
                    kind,
                    field,
                    reason,
                    count: *count,
                })
                .collect();
            return Err(Stop::Fatal(Error::UnsupportedNode { class, count }));
        }
        Ok(tree)
    }

    fn classify(&mut self, error: Error) -> Stop {
        match error {
            // The sentinel answer, or the system no longer knowing the window.
            Error::WindowGone | Error::QueryRejected => Stop::Inconsistent("window_gone"),
            Error::QueryTimeout => match self.check_deadline() {
                Err(truncated) => Stop::Fatal(truncated),
                // No answer in time, or one that arrived after its deadline.
                Ok(()) => Stop::Inconsistent("request_timeout"),
            },
            other => Stop::Fatal(other),
        }
    }

    fn request(&mut self, scope: &Scope, node: i64, prefetch: i32) -> Result<Vec<Node>, Error> {
        self.check_deadline()?;
        let now = self.transport.now();
        let deadline = (now + REQUEST_DEADLINE).min(self.deadline);
        self.stats.requests = self.stats.requests.saturating_add(1);
        self.transport.find(scope.window, node, prefetch, deadline)
    }

    /// Obtain one node: from this pass's prefetched nodes, else from the app.
    fn fetch(
        &mut self,
        scope: &mut Scope,
        node: i64,
        root: bool,
        placed: usize,
        failures: &mut Failures,
    ) -> Result<Fetched, Stop> {
        if !root && let Some(cached) = scope.cache.remove(&node) {
            return Ok(Fetched::Node(Box::new(cached)));
        }
        let nodes = match self.request(scope, node, PREFETCH) {
            Ok(nodes) => {
                self.stats.batches = self.stats.batches.saturating_add(1);
                nodes
            }
            Err(Error::UnsupportedNodeField { .. } | Error::InvalidNodeField { .. }) => {
                // Some node of the answer is undecodable. Nothing of that answer
                // is used; ask for this node alone so the failure is attributed.
                self.stats.single_fallbacks = self.stats.single_fallbacks.saturating_add(1);
                match self.request(scope, node, 0) {
                    Ok(nodes) => nodes,
                    Err(Error::UnsupportedNodeField { field, reason }) => {
                        *failures.entry(("unsupported", field, reason)).or_default() += 1;
                        return Ok(Fetched::Failed);
                    }
                    Err(Error::InvalidNodeField { field, reason }) => {
                        *failures.entry(("invalid", field, reason)).or_default() += 1;
                        return Ok(Fetched::Failed);
                    }
                    Err(error) => return Err(self.classify(error)),
                }
            }
            Err(error) => return Err(self.classify(error)),
        };
        let mut found = None;
        for (position, candidate) in nodes.into_iter().enumerate() {
            // A root request is answered with the root's real id, first in the list.
            let requested = if root {
                position == 0
            } else {
                candidate.source_id == node
            };
            if candidate.window_id != scope.window {
                if requested {
                    return Err(Stop::Fatal(Error::InvalidNodeField {
                        field: "window_id",
                        reason: "request_window_mismatch",
                    }));
                }
                self.stats.foreign_nodes = self.stats.foreign_nodes.saturating_add(1);
            } else if requested && found.is_none() {
                found = Some(candidate);
            } else if !scope.seen.contains(&candidate.source_id) {
                if placed + scope.cache.len() >= MAX_NODES + contract::MAX_BATCH_NODES {
                    return Err(Stop::Fatal(Error::Truncated { limit: "nodes" }));
                }
                scope.cache.entry(candidate.source_id).or_insert(candidate);
            }
        }
        Ok(found.map_or(Fetched::Null, |node| Fetched::Node(Box::new(node))))
    }

    fn place(
        &mut self,
        tree: &mut Tree,
        node: Node,
        index: usize,
        depth: usize,
    ) -> Result<usize, Stop> {
        if tree.arena.len() >= MAX_NODES {
            return Err(Stop::Fatal(Error::Truncated { limit: "nodes" }));
        }
        tree.arena.push(Element {
            node,
            index,
            children: Vec::new(),
        });
        self.stats.nodes += 1;
        self.stats.max_depth = self.stats.max_depth.max(depth as u32);
        Ok(tree.arena.len() - 1)
    }

    /// Walk one window depth first. Returns its root and whether the root
    /// itself was undecodable (which is a failure, not an empty window).
    fn window(
        &mut self,
        window: &Window,
        tree: &mut Tree,
        failures: &mut Failures,
    ) -> Result<(Option<usize>, bool), Stop> {
        let mut scope = Scope {
            window: window.id,
            cache: HashMap::new(),
            seen: HashSet::new(),
        };
        let placed = tree.arena.len();
        let root = match self.fetch(&mut scope, contract::ROOT_NODE, true, placed, failures)? {
            Fetched::Node(node) => *node,
            Fetched::Null => return Ok((None, false)),
            Fetched::Failed => return Ok((None, true)),
        };
        scope.seen.insert(root.source_id);
        let root = self.place(tree, root, 0, 1)?;
        // (element, next child position); its length is the depth of the top.
        let mut stack = vec![(root, 0_usize)];
        while let Some(&(element, position)) = stack.last() {
            let Some(&child) = tree.arena[element].node.children.get(position) else {
                stack.pop();
                continue;
            };
            stack.last_mut().expect("non-empty stack").1 += 1;
            if stack
                .iter()
                .any(|(ancestor, _)| tree.arena[*ancestor].node.source_id == child)
            {
                return Err(Stop::Fatal(Error::InvalidTree("cycle")));
            }
            if scope.seen.contains(&child) {
                // Listed by two parents: emitted once, where it was first reached.
                self.stats.duplicates = self.stats.duplicates.saturating_add(1);
                continue;
            }
            if stack.len() >= MAX_DEPTH {
                return Err(Stop::Fatal(Error::Truncated { limit: "depth" }));
            }
            let placed = tree.arena.len();
            match self.fetch(&mut scope, child, false, placed, failures)? {
                Fetched::Node(node) => {
                    scope.seen.insert(child);
                    let index = self.place(tree, *node, position, stack.len() + 1)?;
                    tree.arena[element].children.push(index);
                    stack.push((index, 0));
                }
                // A parent listed it and the app no longer has it.
                Fetched::Null => return Err(Stop::Inconsistent("child_missing")),
                Fetched::Failed => {
                    scope.seen.insert(child);
                }
            }
        }
        Ok((Some(root), false))
    }
}

fn boolean(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn sizes_text(sizes: &[DisplaySize]) -> String {
    let parts: Vec<String> = sizes
        .iter()
        .map(|(id, size)| match size {
            Some((width, height)) => format!("{id}:{width}x{height}"),
            None => format!("{id}:unknown"),
        })
        .collect();
    parts.join(" ")
}

fn render(tree: &Tree, stats: &Stats, generation: u64) -> Result<(Vec<u8>, u64), Error> {
    let mut out = xml::Writer::new(MAX_XML_BYTES)?;
    out.open("hierarchy")?;
    out.attribute("rotation", "unknown")?;
    out.attribute("snapshot-id", &stats.snapshot_id.to_string())?;
    out.attribute("start-ms", &stats.start_ms.to_string())?;
    out.attribute("end-ms", &stats.end_ms.to_string())?;
    out.attribute("generation", &generation.to_string())?;
    // Window trees in the document; windows with a null root are counted apart.
    let emitted = tree.windows.iter().filter(|w| w.root.is_some()).count();
    out.attribute("windows", &emitted.to_string())?;
    out.attribute("empty-windows", &stats.empty_windows.to_string())?;
    out.attribute("nodes", &stats.nodes.to_string())?;
    out.attribute("displays", &sizes_text(&tree.sizes))?;
    out.close_start()?;
    for entry in &tree.windows {
        let Some(root) = entry.root else { continue };
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let mut next = Some(root);
        loop {
            if let Some(index) = next.take() {
                let element = &tree.arena[index];
                let node = &element.node;
                let text = |value: &Option<String>| value.as_deref().unwrap_or("").to_owned();
                let bounds = node.bounds_in_screen;
                out.open("node")?;
                out.attribute("index", &element.index.to_string())?;
                out.attribute("text", &text(&node.text))?;
                out.attribute("resource-id", &text(&node.resource_id))?;
                out.attribute("class", &text(&node.class))?;
                out.attribute("package", &text(&node.package))?;
                out.attribute("content-desc", &text(&node.content_description))?;
                let p = node.properties;
                out.attribute("checkable", boolean(p.checkable))?;
                out.attribute("checked", boolean(p.checked))?;
                out.attribute("clickable", boolean(p.clickable))?;
                out.attribute("enabled", boolean(p.enabled))?;
                out.attribute("focusable", boolean(p.focusable))?;
                out.attribute("focused", boolean(p.focused))?;
                out.attribute("scrollable", boolean(p.scrollable))?;
                out.attribute("long-clickable", boolean(p.long_clickable))?;
                out.attribute("password", boolean(p.password))?;
                out.attribute("selected", boolean(p.selected))?;
                out.attribute(
                    "bounds",
                    &format!(
                        "[{},{}][{},{}]",
                        bounds.left, bounds.top, bounds.right, bounds.bottom
                    ),
                )?;
                if stack.is_empty() {
                    out.attribute("window-id", &entry.window.id.to_string())?;
                    out.attribute("display-id", &entry.window.display_id.to_string())?;
                    out.attribute("window-type", &entry.window.kind.to_string())?;
                    out.attribute("window-layer", &entry.window.layer.to_string())?;
                }
                if element.children.is_empty() {
                    out.empty()?;
                } else {
                    out.close_start()?;
                    stack.push((index, 0));
                }
            }
            let Some((parent, position)) = stack.last_mut() else {
                break;
            };
            match tree.arena[*parent].children.get(*position) {
                Some(child) => {
                    *position += 1;
                    next = Some(*child);
                }
                None => {
                    stack.pop();
                    out.end("node")?;
                }
            }
        }
    }
    out.end("hierarchy")?;
    Ok(out.finish())
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// One snapshot over a registered session. `Ok` holds the complete document;
/// on any error there is no document at all. The counters describe the last
/// attempt (request totals cover every attempt).
pub(super) fn snapshot(transport: &mut dyn Transport, sequence: &mut Sequence) -> Taken {
    let start = transport.now();
    sequence.last_id += 1;
    let mut stats = Stats {
        snapshot_id: sequence.last_id,
        start_ms: milliseconds(start.saturating_duration_since(sequence.epoch)),
        ..Stats::default()
    };
    let invalidations = transport.invalidations();
    let tree = Walk {
        transport: &mut *transport,
        deadline: start + TOTAL_DEADLINE,
        stats: &mut stats,
    }
    .attempts();
    let end = transport.now();
    stats.end_ms = milliseconds(end.saturating_duration_since(sequence.epoch));
    stats.elapsed_ms = milliseconds(end.saturating_duration_since(start));
    stats.invalidations = transport.invalidations().saturating_sub(invalidations);
    let result = tree.and_then(|tree| {
        let changed = sequence.sizes.as_ref() != Some(&tree.sizes);
        let generation = sequence.generation + u64::from(changed);
        let (document, sanitized) = render(&tree, &stats, generation)?;
        // Only a complete document advances the geometry generation.
        sequence.generation = generation;
        sequence.sizes = Some(tree.sizes);
        stats.generation = generation;
        stats.xml_bytes = Some(document.len() as u64);
        stats.sanitized_characters = sanitized;
        Ok(document)
    });
    (stats, result)
}

/// What the Android session did around the snapshot. Counters and codes only.
#[derive(Debug, Default)]
pub(super) struct SessionOutcome {
    pub(super) registration_flags: Option<i32>,
    pub(super) initialized: bool,
    pub(super) connection_id: Option<i32>,
    pub(super) unregistered: bool,
    pub(super) interrupted: bool,
    pub(super) callbacks: state::Counters,
    pub(super) revocations: u32,
    pub(super) error: Option<Error>,
    pub(super) cleanup_error: Option<Error>,
}

#[derive(Debug, Serialize)]
struct SessionSummary {
    initialized: bool,
    connection_id: Option<i32>,
    unregistered: bool,
    interrupted: bool,
    callbacks: state::Counters,
    revocations: u32,
}

#[derive(Debug, Serialize)]
struct Limits {
    total_deadline_ms: u64,
    request_deadline_ms: u64,
    max_attempts: u32,
    max_nodes: usize,
    max_depth: usize,
    max_xml_bytes: usize,
    max_displays: usize,
    max_windows_per_display: usize,
    max_callback_bytes: i32,
}

/// `hierarchy-probe` document. Serialization never includes the XML.
#[derive(Serialize)]
pub(crate) struct Report {
    schema_version: u32,
    environment: Diagnostics,
    runtime_fingerprint: Option<String>,
    /// Pinned profile the gate selected; null on every other target.
    profile: Option<&'static str>,
    /// Flags sent to `registerUiTestAutomationService`; null when nothing was sent.
    registration_flags: Option<i32>,
    limits: Limits,
    session: SessionSummary,
    snapshot: Option<Stats>,
    /// First failure: session, then snapshot. Decode errors name their field.
    error: Option<Error>,
    cleanup_error: Option<Error>,
    executed: bool,
    #[serde(skip)]
    document: Option<Vec<u8>>,
}

// The document is UI content: keep it out of every formatted output.
impl std::fmt::Debug for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Report {{ profile: {:?}, error: {:?}, snapshot: {:?}, document: [redacted] }}",
            self.profile, self.error, self.snapshot
        )
    }
}

impl Report {
    fn build(
        environment: Diagnostics,
        fingerprint: Option<String>,
        profile: Option<contract::Profile>,
        session: SessionOutcome,
        snapshot: Option<Taken>,
    ) -> Self {
        let executed = snapshot.is_some();
        let (stats, result) = match snapshot {
            Some((stats, result)) => (Some(stats), Some(result)),
            None => (None, None),
        };
        let (document, snapshot_error) = match result {
            Some(Ok(document)) => (Some(document), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
        let mut report = Self {
            schema_version: 1,
            environment,
            runtime_fingerprint: fingerprint,
            profile: profile.map(contract::Profile::name),
            registration_flags: session.registration_flags,
            limits: Limits {
                total_deadline_ms: milliseconds(TOTAL_DEADLINE),
                request_deadline_ms: milliseconds(REQUEST_DEADLINE),
                max_attempts: MAX_ATTEMPTS,
                max_nodes: MAX_NODES,
                max_depth: MAX_DEPTH,
                max_xml_bytes: MAX_XML_BYTES,
                max_displays: contract::MAX_DISPLAYS,
                max_windows_per_display: contract::MAX_WINDOWS_PER_DISPLAY,
                max_callback_bytes: contract::MAX_BATCH_BYTES,
            },
            session: SessionSummary {
                initialized: session.initialized,
                connection_id: session.connection_id,
                unregistered: session.unregistered,
                interrupted: session.interrupted,
                callbacks: session.callbacks,
                revocations: session.revocations,
            },
            snapshot: stats,
            error: session.error.or(snapshot_error),
            cleanup_error: session.cleanup_error,
            executed,
            document,
        };
        // A document is only command data when the whole session was clean.
        if !report.passed() {
            report.document = None;
        }
        report
    }

    /// A complete snapshot from a session that registered and unregistered.
    pub(crate) fn passed(&self) -> bool {
        self.executed
            && self.session.initialized
            && self.session.unregistered
            && self.error.is_none()
            && self.cleanup_error.is_none()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|s| s.xml_bytes.is_some())
    }

    /// The XML document, once; `None` unless [`Self::passed`].
    pub(crate) fn take_document(&mut self) -> Option<Vec<u8>> {
        self.document.take()
    }
}

/// Run one snapshot behind the exact-profile gate. Off the pinned hierarchy
/// profile this sends no Binder transaction and reports `unsupported_profile`.
pub(crate) fn collect() -> Result<Report, CarloError> {
    let environment = Diagnostics::collect()?;
    let fingerprint = super::fingerprint()?;
    let profile = contract::Profile::select(
        environment.shell_api(),
        environment.effective_uid(),
        std::env::consts::ARCH,
        fingerprint.as_deref(),
    );
    let (session, snapshot) = match profile {
        Some(profile) if profile.hierarchy() => run(profile),
        // The API 34 window-enumeration contract is not pinned.
        _ => (
            SessionOutcome {
                error: Some(Error::UnsupportedProfile),
                ..SessionOutcome::default()
            },
            None,
        ),
    };
    Ok(Report::build(
        environment,
        fingerprint,
        profile,
        session,
        snapshot,
    ))
}

/// Counters of a snapshot and its document or typed failure.
pub(super) type Taken = (Stats, Result<Vec<u8>, Error>);
/// What the session did, and the snapshot when one was attempted.
pub(super) type Run = (SessionOutcome, Option<Taken>);

#[cfg(target_os = "android")]
fn run(profile: contract::Profile) -> Run {
    use std::sync::{Mutex, OnceLock};
    // One numbering per process. The epoch is the first use of this clock, which
    // for a one-shot command is the start of its only snapshot.
    static SEQUENCE: OnceLock<Mutex<Sequence>> = OnceLock::new();
    let sequence = SEQUENCE.get_or_init(|| Mutex::new(Sequence::new(Instant::now())));
    let Ok(mut sequence) = sequence.lock() else {
        return (
            SessionOutcome {
                error: Some(Error::Runtime("snapshot_sequence_poisoned")),
                ..SessionOutcome::default()
            },
            None,
        );
    };
    super::native::hierarchy::run(profile, &mut sequence)
}

#[cfg(not(target_os = "android"))]
fn run(_: contract::Profile) -> Run {
    unreachable!("profile requires Android")
}

/// The hierarchy's exact gate: a pinned profile that carries the window-list
/// contract. Pure comparisons; nothing is loaded and no Binder call is made.
fn gated(
    shell_api: Option<u32>,
    effective_uid: Option<u32>,
    architecture: &str,
    fingerprint: Option<&str>,
) -> Option<contract::Profile> {
    contract::Profile::select(shell_api, effective_uid, architecture, fingerprint)
        .filter(|profile| profile.hierarchy())
}

/// Whether this process is on the pinned hierarchy profile (`binder_hierarchy`).
/// False on the host and on the API 34 reference.
pub(crate) fn pinned(environment: &Diagnostics, fingerprint: Option<&str>) -> bool {
    gated(
        environment.shell_api(),
        environment.effective_uid(),
        std::env::consts::ARCH,
        fingerprint,
    )
    .is_some()
}

/// Snapshots for a long-lived caller (`carlo serve`): the gate is decided once
/// and the snapshot id and generation continue across snapshots. Every snapshot
/// registers, reads and unregisters its own session, so nothing is held between
/// snapshots and nothing outlives this value.
pub(crate) struct Snapshots {
    profile: contract::Profile,
    sequence: Sequence,
}

impl Snapshots {
    /// `None` off the pinned hierarchy profile; then nothing was opened.
    pub(crate) fn detect(environment: &Diagnostics, fingerprint: Option<&str>) -> Option<Self> {
        gated(
            environment.shell_api(),
            environment.effective_uid(),
            std::env::consts::ARCH,
            fingerprint,
        )
        .map(|profile| Self {
            profile,
            sequence: Sequence::new(Instant::now()),
        })
    }

    /// One complete document or a typed failure, and whether a termination
    /// signal arrived while the session owned the signal handlers.
    pub(crate) fn take(&mut self) -> (Result<Vec<u8>, Error>, bool) {
        let (session, snapshot) = run_session(self.profile, &mut self.sequence);
        let interrupted = session.interrupted;
        (document(session, snapshot), interrupted)
    }
}

#[cfg(target_os = "android")]
fn run_session(profile: contract::Profile, sequence: &mut Sequence) -> Run {
    super::native::hierarchy::run(profile, sequence)
}

#[cfg(not(target_os = "android"))]
fn run_session(_: contract::Profile, _: &mut Sequence) -> Run {
    (
        SessionOutcome {
            error: Some(Error::UnsupportedProfile),
            ..SessionOutcome::default()
        },
        None,
    )
}

/// The document only when the whole session was clean, as [`Report::passed`]:
/// session error first, then the snapshot's, then the cleanup's.
fn document(session: SessionOutcome, snapshot: Option<Taken>) -> Result<Vec<u8>, Error> {
    if let Some(error) = session.error {
        return Err(error);
    }
    let Some((_, result)) = snapshot else {
        return Err(Error::Runtime("snapshot_not_taken"));
    };
    let document = result?;
    if let Some(error) = session.cleanup_error {
        return Err(error);
    }
    if !(session.initialized && session.unregistered) {
        return Err(Error::Runtime("session_not_clean"));
    }
    Ok(document)
}

#[cfg(test)]
mod serve_gate {
    use super::*;

    const SAMSUNG: Option<&str> = Some(contract::SAMSUNG_A16X_FINGERPRINT);

    #[test]
    fn only_the_pinned_api36_profile_passes_the_hierarchy_gate() {
        assert_eq!(
            gated(Some(36), Some(2000), "aarch64", SAMSUNG),
            Some(contract::Profile::SamsungA16xApi36)
        );
        for (api, uid, arch, fingerprint) in [
            // The API 34 reference has a session profile but no window contract.
            (Some(34), Some(2000), "x86_64", Some(contract::FINGERPRINT)),
            (Some(36), Some(0), "aarch64", SAMSUNG),
            (Some(36), Some(2000), "x86_64", SAMSUNG),
            (Some(35), Some(2000), "aarch64", SAMSUNG),
            (Some(36), Some(2000), "aarch64", None),
            (None, None, "x86_64", None),
        ] {
            assert_eq!(gated(api, uid, arch, fingerprint), None);
        }
        let host = Diagnostics::from_environment("linux", "x86_64", None, None, Some(1000), None);
        assert!(!pinned(&host, SAMSUNG));
        assert!(Snapshots::detect(&host, SAMSUNG).is_none());
        let reference = Diagnostics::from_environment(
            "android",
            "x86_64",
            Some(34),
            Some("x86_64".into()),
            Some(2000),
            None,
        );
        assert!(!pinned(&reference, Some(contract::FINGERPRINT)));
        assert!(Snapshots::detect(&reference, Some(contract::FINGERPRINT)).is_none());
    }

    fn clean() -> SessionOutcome {
        SessionOutcome {
            initialized: true,
            unregistered: true,
            ..SessionOutcome::default()
        }
    }

    #[test]
    fn a_document_is_returned_only_from_a_clean_session() {
        let xml = b"<hierarchy/>".to_vec();
        let taken = || Some((Stats::default(), Ok(xml.clone())));
        assert_eq!(document(clean(), taken()), Ok(xml.clone()));
        let failed = SessionOutcome {
            error: Some(Error::Conflict),
            ..SessionOutcome::default()
        };
        assert_eq!(document(failed, None), Err(Error::Conflict));
        assert_eq!(
            document(clean(), None),
            Err(Error::Runtime("snapshot_not_taken"))
        );
        let truncated = Error::Truncated { limit: "nodes" };
        assert_eq!(
            document(clean(), Some((Stats::default(), Err(truncated.clone())))),
            Err(truncated)
        );
        let leaked = SessionOutcome {
            cleanup_error: Some(Error::Runtime("unregister")),
            ..clean()
        };
        assert_eq!(document(leaked, taken()), Err(Error::Runtime("unregister")));
        let registered = SessionOutcome {
            unregistered: false,
            ..clean()
        };
        assert_eq!(
            document(registered, taken()),
            Err(Error::Runtime("session_not_clean"))
        );
    }
}

/// Runs on the device through the same gate as the commands. On the pinned
/// hierarchy build it registers and reads the screen; everywhere else it proves
/// the refusal. Prints counters only.
#[cfg(all(test, target_os = "android"))]
mod android_snapshot {
    use super::*;

    /// Open descriptors and threads. Binder teardown of a session is
    /// asynchronous, so the lowest of a few spaced samples is used: a leak
    /// persists, a transient descriptor does not.
    fn resources() -> (usize, usize) {
        (0..5)
            .map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                (
                    std::fs::read_dir("/proc/self/fd").unwrap().count(),
                    std::fs::read_dir("/proc/self/task").unwrap().count(),
                )
            })
            .min()
            .unwrap()
    }

    const CYCLES: usize = 8;

    #[test]
    fn android_snapshot_succeeds_or_fails_typed_and_releases_resources() {
        // The reported snapshot. It also loads libraries and starts the pool, so
        // the resource floor is taken after it and checked across a second one.
        let mut report = collect().unwrap();
        let text = serde_json::to_string(&serde_json::json!({
            "profile": report.profile,
            "passed": report.passed(),
            "registration_flags": report.registration_flags,
            "session": report.session,
            "snapshot": report.snapshot,
            "error": report.error,
            "cleanup_error": report.cleanup_error,
        }))
        .unwrap();
        println!("CARLO_HIERARCHY_SNAPSHOT {text}");
        // The second snapshot settles lazily created Binder state; the floor is
        // taken after it and must hold across further cycles.
        let second = collect().unwrap();
        let floor = resources();
        let mut cycles = Vec::new();
        for _ in 0..CYCLES {
            let outcome = collect().unwrap();
            assert_eq!(outcome.passed(), second.passed(), "{outcome:?}");
            cycles.push(resources());
        }
        let after = *cycles.last().unwrap();
        println!("CARLO_HIERARCHY_RESOURCE_CYCLES {floor:?} {cycles:?}");
        for outcome in [&report, &second] {
            assert!(outcome.passed() || outcome.error.is_some(), "{outcome:?}");
            assert!(outcome.cleanup_error.is_none(), "{outcome:?}");
            match outcome.registration_flags {
                Some(flags) => assert_eq!(flags, 1),
                None => assert!(!outcome.session.initialized),
            }
            // A session that initialized must have been unregistered.
            assert!(!outcome.session.initialized || outcome.session.unregistered);
            if outcome.profile != Some("samsung_a16x_api36") {
                assert_eq!(outcome.error, Some(Error::UnsupportedProfile));
                assert!(!outcome.executed && outcome.snapshot.is_none());
            }
        }
        assert_eq!(report.take_document().is_some(), report.passed());
        assert_eq!(after.0, floor.0, "{floor:?} {after:?}");
        assert!(
            after.1 <= floor.1 + crate::binder::pool::THREADS as usize,
            "{floor:?} {after:?}"
        );
    }
}

#[cfg(test)]
mod tests;
