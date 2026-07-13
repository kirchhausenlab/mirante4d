use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{BufRead, BufReader, Read},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::transition::{HOSTED_HIT_PREFIX, READY_PREFIX, StoreTransition, TRACE_PREFIX};

const GUEST_TEST: &str = "actor::tests::durability_tests::project_store_vm_guest_driver";
const CHILD_TIMEOUT: Duration = Duration::from_secs(20);
const MARKER_TIMEOUT: Duration = Duration::from_secs(20);
const WORKERS: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Flow {
    case: &'static str,
    scenario_transition: &'static str,
    scenario_lane: &'static str,
    fixture_store: &'static str,
}

const FLOWS: [Flow; 8] = [
    Flow {
        case: "save-as",
        scenario_transition: "object_file_sync",
        scenario_lane: "none",
        fixture_store: "recoverable.m4dproj",
    },
    Flow {
        case: "manual-save",
        scenario_transition: "recovery_file_sync",
        scenario_lane: "manual",
        fixture_store: "recoverable.m4dproj",
    },
    Flow {
        case: "autosave",
        scenario_transition: "recovery_file_sync",
        scenario_lane: "autosave",
        fixture_store: "stale.m4dproj",
    },
    Flow {
        case: "staging-cleanup",
        scenario_transition: "staging_cleanup_payload_remove",
        scenario_lane: "none",
        fixture_store: "recoverable.m4dproj",
    },
    Flow {
        case: "pin",
        scenario_transition: "pin_file_sync",
        scenario_lane: "none",
        fixture_store: "recoverable.m4dproj",
    },
    Flow {
        case: "unpin",
        scenario_transition: "unpin_remove",
        scenario_lane: "none",
        fixture_store: "recoverable.m4dproj",
    },
    Flow {
        case: "trash",
        scenario_transition: "gc_trash_move",
        scenario_lane: "none",
        fixture_store: "recoverable.m4dproj",
    },
    Flow {
        case: "purge",
        scenario_transition: "purge_remove",
        scenario_lane: "none",
        fixture_store: "recoverable.m4dproj",
    },
];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TransitionRow {
    transition: String,
    lane: String,
    edge: String,
    occurrence: usize,
}

impl TransitionRow {
    fn point(&self) -> TransitionPoint {
        TransitionPoint {
            transition: self.transition.clone(),
            lane: self.lane.clone(),
            occurrence: self.occurrence,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TransitionPoint {
    transition: String,
    lane: String,
    occurrence: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TreeEntry {
    Directory,
    File(Vec<u8>),
}

struct MatrixWorkspace(PathBuf);

impl MatrixWorkspace {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "mirante4d-hosted-durability-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create hosted durability workspace");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for MatrixWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct PreparedRoots {
    base: PathBuf,
    root_a: PathBuf,
    root_b: PathBuf,
}

impl PreparedRoots {
    fn new(workspace: &Path, template: &Path, flow: Flow, label: &str) -> Self {
        let base = workspace.join(label);
        let root_a = base.join("a");
        let root_b = base.join("b");
        fs::create_dir_all(&root_a).expect("create hosted root A");
        fs::create_dir_all(&root_b).expect("create hosted root B");
        copy_tree(
            &template.join(flow.fixture_store),
            &root_a.join("source.m4dproj"),
        );
        Self {
            base,
            root_a,
            root_b,
        }
    }

    fn release_base(&self) -> PathBuf {
        self.base.join("transition-release")
    }
}

impl Drop for PreparedRoots {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn exhaustive_hosted_and_process_transition_matrix() {
    let started = Instant::now();
    let workspace = MatrixWorkspace::new();
    let template = workspace.path().join("template");
    install_templates(&template);

    let traces = discover_rows(workspace.path(), &template);
    let rows = traces.keys().cloned().collect::<Vec<_>>();
    validate_discovery(&rows);

    let inherited_fail_rows = rows
        .iter()
        .filter(|row| has_existing_exact_failure_matrix(&row.transition))
        .count();
    let fail_targets = rows
        .iter()
        .filter(|row| !has_existing_exact_failure_matrix(&row.transition))
        .cloned()
        .collect::<Vec<_>>();
    parallel_for(&fail_targets, |index, row| {
        let flow = FLOWS[*traces.get(row).expect("traced row has a flow")];
        fail_inject_row(workspace.path(), &template, flow, row, index);
    });

    let bracket_targets = bracket_targets(&traces);
    parallel_for(&bracket_targets, |index, (point, flow_index)| {
        bracket_pure_point(
            workspace.path(),
            &template,
            FLOWS[*flow_index],
            point,
            index,
        );
    });

    let kill_targets = rows
        .iter()
        .filter(|row| {
            mutates_filesystem_or_lease(&row.transition)
                && !has_existing_process_kill_matrix(&row.transition)
        })
        .cloned()
        .collect::<Vec<_>>();
    parallel_for(&kill_targets, |index, row| {
        let flow = FLOWS[*traces.get(row).expect("traced row has a flow")];
        kill_reopen_and_retry(workspace.path(), &template, flow, row, index);
    });

    assert_eq!(fail_targets.len() + inherited_fail_rows, rows.len());
    assert!(!bracket_targets.is_empty());
    assert!(!kill_targets.is_empty());
    eprintln!(
        "M4D_HOSTED_TRANSITION_MATRIX_V1 discovered_edge_rows={} newly_fail_injected={} inherited_exact_fail_rows={} pure_before_after_trees={} newly_sigkill_reopen_retry={} inherited_process_matrices=pin_unpin_trash_purge workers={} wall_milliseconds={} power_loss_simulated=false durability_claim=false",
        rows.len(),
        fail_targets.len(),
        inherited_fail_rows,
        bracket_targets.len(),
        kill_targets.len(),
        WORKERS,
        started.elapsed().as_millis(),
    );
}

fn install_templates(template: &Path) {
    fs::create_dir(template).expect("create fixture template root");
    for store in ["recoverable.m4dproj", "stale.m4dproj"] {
        let destination = template.join(store);
        fs::create_dir(&destination).expect("create fixture template destination");
        let output = Command::new("tar")
            .arg("-xzf")
            .arg(super::fixture_archive())
            .arg("-C")
            .arg(&destination)
            .arg("--strip-components=1")
            .arg(store)
            .output()
            .expect("run fixture extraction");
        assert!(
            output.status.success(),
            "failed to extract {store}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn discover_rows(workspace: &Path, template: &Path) -> BTreeMap<TransitionRow, usize> {
    let mut rows = BTreeMap::new();
    for (flow_index, flow) in FLOWS.into_iter().enumerate() {
        let roots = PreparedRoots::new(workspace, template, flow, &format!("trace-{flow_index}"));
        let output = run_child(flow, &roots, "trace", None, None, CHILD_TIMEOUT);
        assert!(
            output.status.success(),
            "{} trace failed: {}",
            flow.case,
            String::from_utf8_lossy(&output.stderr)
        );
        let flow_rows = parse_rows(&output.stdout, TRACE_PREFIX);
        assert!(!flow_rows.is_empty(), "{} trace emitted no rows", flow.case);
        for row in flow_rows {
            rows.entry(row).or_insert(flow_index);
        }
    }
    rows
}

fn validate_discovery(rows: &[TransitionRow]) {
    let observed_names = rows
        .iter()
        .map(|row| row.transition.as_str())
        .collect::<BTreeSet<_>>();
    let expected_names = StoreTransition::ALL
        .into_iter()
        .map(StoreTransition::name)
        .collect::<BTreeSet<_>>();
    assert_eq!(observed_names, expected_names);

    let mut groups: BTreeMap<(&str, &str, &str), BTreeSet<usize>> = BTreeMap::new();
    for row in rows {
        groups
            .entry((&row.transition, &row.lane, &row.edge))
            .or_default()
            .insert(row.occurrence);
    }
    for ((transition, lane, edge), occurrences) in groups {
        let expected = (0..occurrences.len()).collect::<BTreeSet<_>>();
        assert_eq!(
            occurrences, expected,
            "non-contiguous occurrences for {transition}/{lane}/{edge}"
        );
    }

    let points = rows
        .iter()
        .map(TransitionRow::point)
        .collect::<BTreeSet<_>>();
    for point in points {
        for edge in ["before", "after"] {
            assert!(
                rows.iter()
                    .any(|row| row.point() == point && row.edge == edge),
                "missing {edge} edge for {point:?}"
            );
        }
    }
}

fn fail_inject_row(
    workspace: &Path,
    template: &Path,
    flow: Flow,
    row: &TransitionRow,
    index: usize,
) {
    let roots = PreparedRoots::new(workspace, template, flow, &format!("fail-{index:03}"));
    let output = run_child(
        flow,
        &roots,
        "exercise",
        Some(row),
        Some("fail"),
        CHILD_TIMEOUT,
    );
    let hits = parse_rows(&output.stdout, HOSTED_HIT_PREFIX);
    assert_eq!(
        hits.as_slice(),
        std::slice::from_ref(row),
        "wrong hosted fail marker for {row:?}; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bracket_targets(traces: &BTreeMap<TransitionRow, usize>) -> Vec<(TransitionPoint, usize)> {
    let mut candidates: BTreeMap<TransitionPoint, BTreeMap<usize, BTreeSet<&str>>> =
        BTreeMap::new();
    for (row, flow) in traces {
        if is_pure_read_or_comparison(&row.transition) {
            candidates
                .entry(row.point())
                .or_default()
                .entry(*flow)
                .or_default()
                .insert(&row.edge);
        }
    }
    candidates
        .into_iter()
        .map(|(point, flows)| {
            let flow = flows
                .into_iter()
                .find_map(|(flow, edges)| {
                    (edges.contains("before") && edges.contains("after")).then_some(flow)
                })
                .unwrap_or_else(|| panic!("pure point lacks one-flow edge pair: {point:?}"));
            (point, flow)
        })
        .collect()
}

fn bracket_pure_point(
    workspace: &Path,
    template: &Path,
    flow: Flow,
    point: &TransitionPoint,
    index: usize,
) {
    let roots = PreparedRoots::new(workspace, template, flow, &format!("pure-{index:03}"));
    let row = TransitionRow {
        transition: point.transition.clone(),
        lane: point.lane.clone(),
        edge: "before".to_owned(),
        occurrence: point.occurrence,
    };
    let release = roots.release_base();
    let mut child = spawn_parked_child(flow, &roots, &row, "bracket-park", Some(&release));
    child.wait_for_row(HOSTED_HIT_PREFIX, point, "before", "parked");
    let before = byte_tree(&roots.root_a, &roots.root_b);
    fs::write(release_with_edge(&release, "before"), b"").expect("release before transition edge");
    child.wait_for_row(HOSTED_HIT_PREFIX, point, "after", "parked");
    let after = byte_tree(&roots.root_a, &roots.root_b);
    assert_eq!(
        after, before,
        "pure transition changed project bytes: {point:?}"
    );
    let status = child.kill_and_wait();
    assert_eq!(status.signal(), Some(9));
}

fn kill_reopen_and_retry(
    workspace: &Path,
    template: &Path,
    flow: Flow,
    row: &TransitionRow,
    index: usize,
) {
    let roots = PreparedRoots::new(workspace, template, flow, &format!("kill-{index:03}"));
    let mut child = spawn_parked_child(flow, &roots, row, "park", None);
    child.wait_for_row(READY_PREFIX, &row.point(), &row.edge, "ready");
    let status = child.kill_and_wait();
    assert_eq!(status.signal(), Some(9), "target child was not SIGKILLed");

    let output = run_child(flow, &roots, "validate", None, None, CHILD_TIMEOUT);
    assert!(
        output.status.success(),
        "fresh reopen/retry failed for {row:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_guest_passed(&output.stdout, flow.case, "validate");
}

fn parallel_for<T, F>(items: &[T], operation: F)
where
    T: Sync,
    F: Fn(usize, &T) + Sync,
{
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let operation = &operation;
        for _ in 0..WORKERS.min(items.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    operation(index, item);
                }
            });
        }
    });
}

fn child_command(flow: Flow, roots: &PreparedRoots, role: &str) -> Command {
    let mut command = Command::new(env::current_exe().expect("locate project-store test binary"));
    command
        .arg(GUEST_TEST)
        .arg("--ignored")
        .arg("--exact")
        .arg("--nocapture")
        .env("MIRANTE4D_PROJECT_STORE_VM_ROLE", role)
        .env("MIRANTE4D_PROJECT_STORE_VM_CASE", flow.case)
        .env(
            "MIRANTE4D_PROJECT_STORE_VM_TRANSITION",
            flow.scenario_transition,
        )
        .env("MIRANTE4D_PROJECT_STORE_VM_LANE", flow.scenario_lane)
        .env("MIRANTE4D_PROJECT_STORE_VM_ROOT_A", &roots.root_a)
        .env("MIRANTE4D_PROJECT_STORE_VM_ROOT_B", &roots.root_b);
    command
}

fn set_target(command: &mut Command, row: &TransitionRow, action: &str) {
    command
        .env("MIRANTE4D_PROJECT_STORE_HOSTED_TRANSITION", &row.transition)
        .env("MIRANTE4D_PROJECT_STORE_HOSTED_LANE", &row.lane)
        .env("MIRANTE4D_PROJECT_STORE_HOSTED_EDGE", &row.edge)
        .env(
            "MIRANTE4D_PROJECT_STORE_HOSTED_OCCURRENCE",
            row.occurrence.to_string(),
        )
        .env("MIRANTE4D_PROJECT_STORE_TRANSITION_ACTION", action);
}

fn run_child(
    flow: Flow,
    roots: &PreparedRoots,
    role: &str,
    target: Option<&TransitionRow>,
    action: Option<&str>,
    timeout: Duration,
) -> CapturedOutput {
    let mut command = child_command(flow, roots, role);
    if let Some(target) = target {
        set_target(
            &mut command,
            target,
            action.expect("a transition target requires an action"),
        );
    }
    run_captured(&mut command, timeout)
}

struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_captured(command: &mut Command, timeout: Duration) -> CapturedOutput {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn project-store child");
    let mut stdout = child.stdout.take().expect("capture child stdout");
    let mut stderr = child.stderr.take().expect("capture child stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read child stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read child stderr");
        bytes
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll project-store child") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("project-store child exceeded {timeout:?}");
        }
        thread::sleep(Duration::from_millis(2));
    };
    CapturedOutput {
        status,
        stdout: stdout_reader.join().expect("join stdout reader"),
        stderr: stderr_reader.join().expect("join stderr reader"),
    }
}

struct ParkedChild {
    child: Option<Child>,
    lines: mpsc::Receiver<String>,
    stdout_reader: Option<thread::JoinHandle<()>>,
    stderr_reader: Option<thread::JoinHandle<Vec<u8>>>,
}

impl ParkedChild {
    fn wait_for_row(&self, prefix: &str, point: &TransitionPoint, edge: &str, status: &str) {
        let deadline = Instant::now() + MARKER_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(remaining).unwrap_or_else(|error| {
                panic!("child did not emit {point:?}/{edge}/{status}: {error}")
            });
            let Some(value) = json_after_prefix(&line, prefix) else {
                continue;
            };
            let row = row_from_value(&value);
            if row.point() == *point && row.edge == edge && value["status"].as_str() == Some(status)
            {
                return;
            }
        }
    }

    fn kill_and_wait(&mut self) -> ExitStatus {
        let mut child = self.child.take().expect("child is live");
        child.kill().expect("SIGKILL transition child");
        let status = child.wait().expect("wait for SIGKILL transition child");
        self.stdout_reader
            .take()
            .expect("stdout reader is live")
            .join()
            .expect("join parked stdout reader");
        let _ = self
            .stderr_reader
            .take()
            .expect("stderr reader is live")
            .join()
            .expect("join parked stderr reader");
        status
    }
}

impl Drop for ParkedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.stdout_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}

fn spawn_parked_child(
    flow: Flow,
    roots: &PreparedRoots,
    row: &TransitionRow,
    action: &str,
    release: Option<&Path>,
) -> ParkedChild {
    let mut command = child_command(flow, roots, "exercise");
    set_target(&mut command, row, action);
    if let Some(release) = release {
        command.env("MIRANTE4D_PROJECT_STORE_TRANSITION_RELEASE", release);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn parked project-store child");
    let stdout = child.stdout.take().expect("capture parked child stdout");
    let mut stderr = child.stderr.take().expect("capture parked child stderr");
    let (send, lines) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("read parked child stdout line");
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .read_to_end(&mut bytes)
            .expect("read parked child stderr");
        bytes
    });
    ParkedChild {
        child: Some(child),
        lines,
        stdout_reader: Some(stdout_reader),
        stderr_reader: Some(stderr_reader),
    }
}

fn parse_rows(bytes: &[u8], prefix: &str) -> Vec<TransitionRow> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| json_after_prefix(line, prefix))
        .map(|value| row_from_value(&value))
        .collect()
}

fn json_after_prefix(line: &str, prefix: &str) -> Option<serde_json::Value> {
    let encoded = line.split_once(prefix)?.1;
    Some(serde_json::from_str(encoded).expect("decode transition marker"))
}

fn row_from_value(value: &serde_json::Value) -> TransitionRow {
    TransitionRow {
        transition: value["transition"]
            .as_str()
            .expect("transition marker name")
            .to_owned(),
        lane: value["lane"]
            .as_str()
            .expect("transition marker lane")
            .to_owned(),
        edge: value["edge"]
            .as_str()
            .expect("transition marker edge")
            .to_owned(),
        occurrence: value["occurrence"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .expect("transition marker occurrence"),
    }
}

fn assert_guest_passed(bytes: &[u8], case: &str, role: &str) {
    let prefix = "mirante4d-project-store-vm-result:";
    let results = String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| json_after_prefix(line, prefix))
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1, "guest emitted the wrong result count");
    assert_eq!(results[0]["case"].as_str(), Some(case));
    assert_eq!(results[0]["role"].as_str(), Some(role));
    assert_eq!(results[0]["status"].as_str(), Some("passed"));
}

fn has_existing_exact_failure_matrix(transition: &str) -> bool {
    transition.starts_with("pin_")
        || transition.starts_with("unpin_")
        || transition.starts_with("gc_")
        || transition.starts_with("purge_")
}

fn has_existing_process_kill_matrix(transition: &str) -> bool {
    has_existing_exact_failure_matrix(transition)
}

fn is_pure_read_or_comparison(transition: &str) -> bool {
    matches!(
        transition,
        "envelope_read"
            | "ref_read"
            | "generation_validate"
            | "payload_binding_validate"
            | "object_inventory"
            | "writer_lease_confirm"
            | "expected_parent_check"
            | "gc_root_scan"
            | "gc_candidate_listing"
    )
}

fn mutates_filesystem_or_lease(transition: &str) -> bool {
    !is_pure_read_or_comparison(transition)
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination)
        .unwrap_or_else(|error| panic!("create fixture copy {}: {error}", destination.display()));
    let mut entries = fs::read_dir(source)
        .expect("read fixture copy source")
        .map(|entry| entry.expect("read fixture copy entry"))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source = entry.path();
        let destination = destination.join(entry.file_name());
        let file_type = entry.file_type().expect("read fixture copy type");
        if file_type.is_dir() {
            copy_tree(&source, &destination);
        } else {
            assert!(file_type.is_file(), "fixture template contains a link");
            fs::copy(&source, &destination).expect("copy fixture file");
        }
    }
}

fn byte_tree(root_a: &Path, root_b: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(
        label: &Path,
        root: &Path,
        directory: &Path,
        output: &mut BTreeMap<PathBuf, TreeEntry>,
    ) {
        let relative = directory
            .strip_prefix(root)
            .expect("tree path stays rooted");
        output.insert(label.join(relative), TreeEntry::Directory);
        let mut entries = fs::read_dir(directory)
            .expect("read hosted tree")
            .map(|entry| entry.expect("read hosted tree entry"))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type().expect("read hosted tree type");
            if file_type.is_dir() {
                visit(label, root, &path, output);
            } else {
                assert!(file_type.is_file(), "hosted tree contains a link");
                let relative = path.strip_prefix(root).expect("tree file stays rooted");
                output.insert(
                    label.join(relative),
                    TreeEntry::File(fs::read(path).expect("read hosted tree file")),
                );
            }
        }
    }

    let mut output = BTreeMap::new();
    visit(Path::new("a"), root_a, root_a, &mut output);
    visit(Path::new("b"), root_b, root_b, &mut output);
    output
}

fn release_with_edge(base: &Path, edge: &str) -> PathBuf {
    let mut value = base.as_os_str().to_owned();
    value.push(".");
    value.push(edge);
    PathBuf::from(value)
}
