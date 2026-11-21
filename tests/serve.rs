//! Process-level contract of `carlo serve` on the host: a real binary, a real
//! Unix socket in a private directory and a std client. No device is involved.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use carlo::protocol::{self, Frame, Kind};
use serde_json::{Value, json};

/// Generous bound for anything that should be immediate; far below the agent's
/// own 30 s deadlines, which no test waits for.
const PROMPT: Duration = Duration::from_secs(10);

/// A private session directory, as the host would create it.
struct SessionDir(PathBuf);

impl SessionDir {
    fn new(mode: u32) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "carlo-serve-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        Self(path)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("control.sock")
    }

    fn entries(&self) -> usize {
        fs::read_dir(&self.0).unwrap().count()
    }
}

impl Drop for SessionDir {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Exit {
    code: Option<i32>,
    stdout_rest: String,
    stderr: String,
}

/// A running agent that printed its ready line. Killed on drop if still alive.
struct Agent {
    child: Child,
    stdout: BufReader<ChildStdout>,
    session: String,
    socket: PathBuf,
}

fn spawn(arguments: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_carlo"))
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn finish(mut child: Child, mut stdout: impl Read) -> Exit {
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > PROMPT {
            let _ = child.kill();
            let _ = child.wait();
            panic!("agent did not exit promptly");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stdout_rest = String::new();
    stdout.read_to_string(&mut stdout_rest).unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    Exit {
        code: status.code(),
        stdout_rest,
        stderr,
    }
}

/// Run an invocation that must refuse before becoming ready.
fn refused(arguments: &[&str]) -> Exit {
    let mut child = spawn(arguments);
    let stdout = child.stdout.take().unwrap();
    let exit = finish(child, stdout);
    assert!(
        exit.stdout_rest.is_empty(),
        "refusal must not print a ready line"
    );
    assert!(!exit.stderr.is_empty());
    exit
}

impl Agent {
    fn start(directory: &SessionDir) -> Self {
        let mut child = spawn(&["serve", "--session-dir", directory.0.to_str().unwrap()]);
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("one JSON ready line");
        let socket = directory.socket();
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
        assert_eq!(
            ready,
            json!({
                "ready": true,
                "schema_version": 1,
                "protocol": 1,
                "transport": "filesystem",
                "socket": socket.to_str().unwrap(),
                "session": ready["session"],
            })
        );
        let session = ready["session"].as_str().unwrap().to_owned();
        assert_eq!(session.len(), 32);
        assert!(
            session
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
        Self {
            child,
            stdout,
            session,
            socket,
        }
    }

    fn connect(&self) -> UnixStream {
        let stream = UnixStream::connect(&self.socket).unwrap();
        stream.set_read_timeout(Some(PROMPT)).unwrap();
        stream.set_write_timeout(Some(PROMPT)).unwrap();
        stream
    }

    fn hello_body(&self) -> String {
        json!({ "command": "hello", "protocol": 1, "session": self.session }).to_string()
    }

    /// Wait for the exit; the ready line must have been the only stdout data and
    /// the session identifier must never reach stderr.
    fn exit(self) -> Exit {
        let Self {
            child,
            stdout,
            session,
            socket,
        } = self;
        let exit = finish(child, stdout);
        assert!(exit.stdout_rest.is_empty(), "stdout after the ready line");
        assert!(
            !exit.stderr.contains(&session),
            "session id leaked to stderr"
        );
        assert!(!socket.exists(), "socket path survived the agent");
        exit
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .args([name, &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }
}

fn request(stream: &mut UnixStream, id: u32, body: &str) -> Value {
    protocol::write_frame(stream, Kind::Control, id, body.as_bytes()).unwrap();
    response(stream, id)
}

fn response(stream: &mut UnixStream, id: u32) -> Value {
    let frame: Frame = protocol::read_frame(stream)
        .unwrap()
        .expect("a response frame");
    assert_eq!((frame.kind, frame.request_id), (Kind::Control, id));
    serde_json::from_slice(&frame.payload).unwrap()
}

fn closed(stream: &mut UnixStream) {
    assert!(
        protocol::read_frame(stream).unwrap().is_none(),
        "expected EOF"
    );
}

fn header(kind: u8, id: u32, length: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    protocol::write_frame(&mut bytes, Kind::Control, id, b"").unwrap();
    bytes[5] = kind;
    bytes[12..].copy_from_slice(&length.to_be_bytes());
    bytes
}

#[test]
fn happy_path_handshake_commands_and_shutdown_exit_zero() {
    let directory = SessionDir::new(0o700);
    let agent = Agent::start(&directory);
    let mut stream = agent.connect();
    let hello = request(&mut stream, 0x0102_0304, &agent.hello_body());
    assert_eq!(hello["ok"], true);
    assert_eq!(hello["protocol"], 1);
    assert_eq!(
        hello["agent"],
        json!({"name": "carlo", "version": env!("CARGO_PKG_VERSION")})
    );
    assert_eq!(hello["device"]["architecture"], std::env::consts::ARCH);
    if cfg!(not(target_os = "android")) {
        assert!(hello["device"]["android_api"].is_null() && hello["device"]["abi"].is_null());
    }
    assert_eq!(
        hello["limits"],
        json!({"control_bytes": 65536, "payload_bytes": 8_388_608})
    );
    // The same list `doctor` prints, and nothing is advertised as available.
    let doctor = Command::new(env!("CARGO_BIN_EXE_carlo"))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(hello["capabilities"], doctor["capabilities"]);
    assert!(
        hello["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|capability| capability["available"] == false)
    );

    assert_eq!(
        request(&mut stream, 2, r#"{"command":"ping"}"#),
        json!({"ok": true})
    );
    assert_eq!(
        request(&mut stream, 3, r#"{"command":"capabilities"}"#),
        json!({"ok": true, "capabilities": doctor["capabilities"]})
    );
    let code = |value: Value| value["error"]["code"].as_str().unwrap().to_owned();
    assert_eq!(
        code(request(&mut stream, 4, &agent.hello_body())),
        "already_initialized"
    );
    for name in [
        "hierarchy.dump",
        "display.info",
        "video.start",
        "video.stop",
        "stats",
    ] {
        let body = json!({ "command": name }).to_string();
        assert_eq!(code(request(&mut stream, 5, &body)), "unavailable");
    }
    assert_eq!(
        code(request(&mut stream, 6, r#"{"command":"SENTINEL_X"}"#)),
        "unknown_command"
    );
    assert_eq!(code(request(&mut stream, 7, "not json")), "invalid_request");
    assert_eq!(code(request(&mut stream, 8, "[1]")), "invalid_request");
    protocol::write_frame(&mut stream, Kind::Video, 9, &[0, 0, 1]).unwrap();
    assert_eq!(code(response(&mut stream, 9)), "unexpected_kind");
    assert_eq!(
        request(&mut stream, 10, r#"{"command":"shutdown"}"#),
        json!({"ok": true})
    );
    closed(&mut stream);
    let exit = agent.exit();
    assert_eq!(exit.code, Some(0));
    assert!(exit.stderr.is_empty());
    assert_eq!(directory.entries(), 0);
}

#[test]
fn requests_fragmented_one_byte_at_a_time_are_reassembled() {
    let directory = SessionDir::new(0o700);
    let agent = Agent::start(&directory);
    let mut stream = agent.connect();
    let mut bytes = Vec::new();
    protocol::write_frame(&mut bytes, Kind::Control, 21, agent.hello_body().as_bytes()).unwrap();
    protocol::write_frame(&mut bytes, Kind::Control, 22, br#"{"command":"ping"}"#).unwrap();
    for (index, byte) in bytes.iter().enumerate() {
        stream.write_all(std::slice::from_ref(byte)).unwrap();
        if index % 16 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(response(&mut stream, 21)["ok"], true);
    assert_eq!(response(&mut stream, 22), json!({"ok": true}));
    assert_eq!(
        request(&mut stream, 23, r#"{"command":"shutdown"}"#),
        json!({"ok": true})
    );
    assert_eq!(agent.exit().code, Some(0));
}

#[test]
fn socket_is_private_single_use_and_removed_once_the_client_is_accepted() {
    let directory = SessionDir::new(0o700);
    let agent = Agent::start(&directory);
    let metadata = fs::symlink_metadata(&agent.socket).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
    assert_eq!(directory.entries(), 1);
    assert_eq!(
        fs::metadata(&directory.0).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let mut stream = agent.connect();
    assert_eq!(request(&mut stream, 1, &agent.hello_body())["ok"], true);
    // The response proves the accept happened: the path must already be gone.
    assert!(!agent.socket.exists());
    assert_eq!(directory.entries(), 0);
    assert!(UnixStream::connect(&agent.socket).is_err());
    assert_eq!(
        request(&mut stream, 2, r#"{"command":"ping"}"#),
        json!({"ok": true})
    );
    drop(stream);
    assert_eq!(agent.exit().code, Some(0));
    assert!(directory.0.is_dir(), "the directory belongs to the host");
}

#[test]
fn handshake_failures_answer_with_a_stable_code_then_close_non_zero() {
    let wrong = json!({"command": "hello", "protocol": 1, "session": "0".repeat(32)});
    let cases = [
        (wrong.to_string(), json!({"code": "invalid_session"})),
        (
            r#"{"command":"hello","protocol":1}"#.to_owned(),
            json!({"code": "invalid_session"}),
        ),
        (
            r#"{"command":"ping"}"#.to_owned(),
            json!({"code": "handshake_required"}),
        ),
        (
            r#"{"command":"shutdown"}"#.to_owned(),
            json!({"code": "handshake_required"}),
        ),
    ];
    for (body, error) in cases {
        let directory = SessionDir::new(0o700);
        let agent = Agent::start(&directory);
        let mut stream = agent.connect();
        let reply = request(&mut stream, 31, &body);
        assert_eq!(reply, json!({"ok": false, "error": error}));
        closed(&mut stream);
        let exit = agent.exit();
        assert_eq!(exit.code, Some(74));
        assert!(exit.stderr.contains(error["code"].as_str().unwrap()));
    }
    // Version is checked before the session, with or without a valid one.
    for session in [None, Some(())] {
        let directory = SessionDir::new(0o700);
        let agent = Agent::start(&directory);
        let mut stream = agent.connect();
        let session = session.map_or("x".to_owned(), |()| agent.session.clone());
        let body = json!({"command": "hello", "protocol": 2, "session": session}).to_string();
        assert_eq!(
            request(&mut stream, 32, &body),
            json!({"ok": false, "error": {"code": "unsupported_protocol", "supported": [1]}})
        );
        closed(&mut stream);
        let exit = agent.exit();
        assert_eq!(exit.code, Some(74));
        assert!(exit.stderr.contains("unsupported_protocol"));
    }
}

#[test]
fn malformed_first_requests_do_not_authenticate_and_hello_is_still_required() {
    let directory = SessionDir::new(0o700);
    let agent = Agent::start(&directory);
    let mut stream = agent.connect();
    assert_eq!(
        request(&mut stream, 1, "{")["error"]["code"],
        "invalid_request"
    );
    protocol::write_frame(&mut stream, Kind::Hierarchy, 2, b"<x/>").unwrap();
    assert_eq!(response(&mut stream, 2)["error"]["code"], "unexpected_kind");
    assert_eq!(
        request(&mut stream, 3, r#"{"command":"capabilities"}"#)["error"]["code"],
        "handshake_required"
    );
    closed(&mut stream);
    assert_eq!(agent.exit().code, Some(74));
}

#[test]
fn framing_violations_get_a_rejection_frame_and_a_closed_connection() {
    let mut bad_magic = header(1, 5, 0);
    bad_magic[..4].copy_from_slice(b"HTTP");
    let mut other_version = header(1, 5, 0);
    other_version[4] = 2;
    let mut reserved = header(1, 5, 0);
    reserved[7] = 1;
    let cases = [
        (bad_magic, json!({"error": {"code": "bad_magic"}})),
        (
            other_version,
            json!({"error": {"code": "unsupported_version", "supported_version": 1}}),
        ),
        (reserved, json!({"error": {"code": "reserved_bits"}})),
        (
            header(1, 5, 65537),
            json!({"error": {"code": "frame_too_large", "limit": 65536}}),
        ),
        (
            header(2, 5, u32::MAX),
            json!({"error": {"code": "frame_too_large", "limit": 8_388_608}}),
        ),
    ];
    for after_hello in [false, true] {
        for (bytes, rejection) in &cases {
            let directory = SessionDir::new(0o700);
            let agent = Agent::start(&directory);
            let mut stream = agent.connect();
            if after_hello {
                assert_eq!(request(&mut stream, 1, &agent.hello_body())["ok"], true);
            }
            stream.write_all(bytes).unwrap();
            assert_eq!(&response(&mut stream, 0), rejection);
            closed(&mut stream);
            let exit = agent.exit();
            assert_eq!(exit.code, Some(74));
            assert!(
                exit.stderr
                    .contains(rejection["error"]["code"].as_str().unwrap())
            );
        }
    }
}

#[test]
fn unknown_frame_kind_is_rejected_and_the_connection_still_works() {
    let directory = SessionDir::new(0o700);
    let agent = Agent::start(&directory);
    let mut stream = agent.connect();
    let mut unknown = header(200, 41, 5);
    unknown.extend_from_slice(b"later");
    stream.write_all(&unknown).unwrap();
    assert_eq!(
        response(&mut stream, 41),
        json!({"error": {"code": "unknown_kind"}})
    );
    assert_eq!(request(&mut stream, 42, &agent.hello_body())["ok"], true);
    stream.write_all(&unknown).unwrap();
    assert_eq!(
        response(&mut stream, 41),
        json!({"error": {"code": "unknown_kind"}})
    );
    assert_eq!(
        request(&mut stream, 43, r#"{"command":"ping"}"#),
        json!({"ok": true})
    );
    assert_eq!(
        request(&mut stream, 44, r#"{"command":"shutdown"}"#),
        json!({"ok": true})
    );
    assert_eq!(agent.exit().code, Some(0));
}

#[test]
fn client_eof_is_clean_only_on_a_frame_boundary_after_the_handshake() {
    // Before the handshake: not a session, non-zero.
    let directory = SessionDir::new(0o700);
    let agent = Agent::start(&directory);
    drop(agent.connect());
    let exit = agent.exit();
    assert_eq!(exit.code, Some(74));
    assert!(exit.stderr.contains("before the handshake"));

    // After the handshake, on a boundary: clean.
    let directory = SessionDir::new(0o700);
    let agent = Agent::start(&directory);
    let mut stream = agent.connect();
    assert_eq!(request(&mut stream, 1, &agent.hello_body())["ok"], true);
    drop(stream);
    let exit = agent.exit();
    assert_eq!(exit.code, Some(0));
    assert!(exit.stderr.is_empty());

    // Inside a header or a payload, in either phase: truncated, non-zero.
    for (after_hello, cut) in [(false, 7), (true, 7), (true, 20)] {
        let directory = SessionDir::new(0o700);
        let agent = Agent::start(&directory);
        let mut stream = agent.connect();
        if after_hello {
            assert_eq!(request(&mut stream, 1, &agent.hello_body())["ok"], true);
        }
        let mut bytes = Vec::new();
        protocol::write_frame(&mut bytes, Kind::Control, 2, br#"{"command":"ping"}"#).unwrap();
        stream.write_all(&bytes[..cut]).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        closed(&mut stream);
        let exit = agent.exit();
        assert_eq!(exit.code, Some(74));
        assert!(exit.stderr.contains("truncated_frame"));
    }
}

#[test]
fn unusable_session_directories_are_refused_and_nothing_is_created() {
    for mode in [0o755, 0o750, 0o701, 0o777] {
        let directory = SessionDir::new(mode);
        let exit = refused(&["serve", "--session-dir", directory.0.to_str().unwrap()]);
        assert_eq!(exit.code, Some(69), "mode {mode:o}");
        assert!(exit.stderr.contains("mode must be 0700"));
        assert_eq!(directory.entries(), 0);
        assert_eq!(
            fs::metadata(&directory.0).unwrap().permissions().mode() & 0o777,
            mode,
            "permissions are never changed"
        );
    }
    let directory = SessionDir::new(0o700);
    let missing = directory.0.join("missing");
    let exit = refused(&["serve", "--session-dir", missing.to_str().unwrap()]);
    assert_eq!(exit.code, Some(69));
    assert!(!missing.exists());

    let file = directory.0.join("file");
    fs::write(&file, b"").unwrap();
    assert_eq!(
        refused(&["serve", "--session-dir", file.to_str().unwrap()]).code,
        Some(69)
    );

    let target = SessionDir::new(0o700);
    let link = directory.0.join("link");
    std::os::unix::fs::symlink(&target.0, &link).unwrap();
    let exit = refused(&["serve", "--session-dir", link.to_str().unwrap()]);
    assert_eq!(exit.code, Some(69));
    assert!(exit.stderr.contains("not a real directory"));
    assert_eq!(target.entries(), 0);

    let long = directory.0.join("d".repeat(100));
    fs::create_dir(&long).unwrap();
    fs::set_permissions(&long, fs::Permissions::from_mode(0o700)).unwrap();
    let exit = refused(&["serve", "--session-dir", long.to_str().unwrap()]);
    assert_eq!(exit.code, Some(69));
    assert!(exit.stderr.contains("107 bytes"));
    assert_eq!(fs::read_dir(&long).unwrap().count(), 0);
}

#[test]
fn existing_socket_path_is_refused_and_left_untouched() {
    let directory = SessionDir::new(0o700);
    fs::write(directory.socket(), b"not ours").unwrap();
    let exit = refused(&["serve", "--session-dir", directory.0.to_str().unwrap()]);
    assert_eq!(exit.code, Some(69));
    assert!(exit.stderr.contains("already exists"));
    assert_eq!(fs::read(directory.socket()).unwrap(), b"not ours");

    // A dangling symlink is not followed or replaced either.
    let directory = SessionDir::new(0o700);
    let elsewhere = directory.0.join("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, directory.socket()).unwrap();
    let exit = refused(&["serve", "--session-dir", directory.0.to_str().unwrap()]);
    assert_eq!(exit.code, Some(69));
    assert!(!elsewhere.exists());
    assert!(
        fs::symlink_metadata(directory.socket())
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // A live agent's socket is not stolen by a second one in the same directory.
    let directory = SessionDir::new(0o700);
    let agent = Agent::start(&directory);
    let exit = refused(&["serve", "--session-dir", directory.0.to_str().unwrap()]);
    assert_eq!(exit.code, Some(69));
    let mut stream = agent.connect();
    assert_eq!(request(&mut stream, 1, &agent.hello_body())["ok"], true);
    assert_eq!(
        request(&mut stream, 2, r#"{"command":"shutdown"}"#),
        json!({"ok": true})
    );
    assert_eq!(agent.exit().code, Some(0));
}

#[test]
fn serve_arguments_are_strict_and_never_echoed() {
    for arguments in [
        vec!["serve"],
        vec!["serve", "--session-dir"],
        vec!["serve", "--session-dir", "relative-sensitive-value"],
        vec!["serve", "--session-dir", "./relative-sensitive-value"],
        vec!["serve", "--session-dir", "/tmp/sensitive-value", "extra"],
        vec!["serve", "/tmp/sensitive-value"],
        vec!["serve", "--json"],
    ] {
        let exit = refused(&arguments);
        assert_eq!(exit.code, Some(64), "{arguments:?}");
        assert!(!exit.stderr.contains("sensitive-value"));
    }
    assert!(!Path::new("relative-sensitive-value").exists());
}

#[test]
fn termination_signals_end_the_agent_promptly_and_leave_no_socket() {
    // While waiting for a client.
    for name in ["-TERM", "-INT", "-HUP"] {
        let directory = SessionDir::new(0o700);
        let agent = Agent::start(&directory);
        assert!(agent.socket.exists());
        let started = Instant::now();
        agent.signal(name);
        let exit = agent.exit();
        assert_eq!(exit.code, Some(74), "{name}: handled, not killed");
        assert!(exit.stderr.contains("interrupted"));
        assert!(started.elapsed() < Duration::from_secs(3), "{name}");
        assert_eq!(directory.entries(), 0);
    }
    // With a client attached, before and after the handshake.
    for after_hello in [false, true] {
        let directory = SessionDir::new(0o700);
        let agent = Agent::start(&directory);
        let mut stream = agent.connect();
        if after_hello {
            assert_eq!(request(&mut stream, 1, &agent.hello_body())["ok"], true);
        } else {
            stream.write_all(b"CRLO").unwrap();
        }
        let started = Instant::now();
        agent.signal("-TERM");
        // EOF, or a reset when the signal won the race against the accept.
        match protocol::read_frame(&mut stream) {
            Ok(None) => {}
            Err(protocol::Error::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            other => panic!("expected a closed connection, got {other:?}"),
        }
        let exit = agent.exit();
        assert_eq!(exit.code, Some(74));
        assert!(exit.stderr.contains("interrupted"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

/// The fallback, end to end, with a naturally denied bind: a 0500 directory
/// passes validation (private, owned) but its owner cannot create entries.
/// Root bypasses that permission check, so the test does not apply to it.
#[cfg(target_os = "linux")]
#[test]
fn denied_socket_file_falls_back_to_a_random_abstract_socket() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr;

    let probe = SessionDir::new(0o500);
    if fs::write(probe.0.join("probe"), b"").is_ok() {
        eprintln!("skipped: this user can write to a 0500 directory (root)");
        return;
    }
    let mut names = Vec::new();
    for round in 0..2 {
        let directory = SessionDir::new(0o500);
        let mut child = spawn(&["serve", "--session-dir", directory.0.to_str().unwrap()]);
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("one JSON ready line");
        assert_eq!(
            ready,
            json!({
                "ready": true,
                "schema_version": 1,
                "protocol": 1,
                "transport": "abstract",
                "socket": ready["socket"],
                "session": ready["session"],
            })
        );
        let name = ready["socket"].as_str().unwrap().to_owned();
        let session = ready["session"].as_str().unwrap().to_owned();
        let hex = name.strip_prefix("carlo-").expect("carlo-<hex> name");
        assert_eq!(hex.len(), 32);
        assert!(
            hex.bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
        assert_ne!(
            hex, session,
            "the name must not reveal the session identifier"
        );
        assert_eq!(directory.entries(), 0, "no socket file in the directory");

        let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let mut stream = UnixStream::connect_addr(&address).unwrap();
        stream.set_read_timeout(Some(PROMPT)).unwrap();
        let hello = json!({ "command": "hello", "protocol": 1, "session": session }).to_string();
        let code = if round == 0 {
            assert_eq!(request(&mut stream, 1, &hello)["ok"], true);
            // Accepted: the listener is closed, the name is free, nobody attaches.
            assert!(UnixStream::connect_addr(&address).is_err());
            assert_eq!(
                request(&mut stream, 2, r#"{"command":"ping"}"#),
                json!({"ok": true})
            );
            assert_eq!(
                request(&mut stream, 3, r#"{"command":"shutdown"}"#),
                json!({"ok": true})
            );
            0
        } else {
            // Knowing the name is not enough: the session identifier is required.
            let wrong = json!({ "command": "hello", "protocol": 1, "session": hex }).to_string();
            assert_eq!(
                request(&mut stream, 1, &wrong)["error"]["code"],
                "invalid_session"
            );
            74
        };
        closed(&mut stream);
        let exit = finish(child, stdout);
        assert_eq!(exit.code, Some(code));
        assert!(exit.stdout_rest.is_empty());
        assert!(!exit.stderr.contains(&session));
        assert!(UnixStream::connect_addr(&address).is_err());
        assert_eq!(directory.entries(), 0);
        names.push(name);
    }
    assert_ne!(names[0], names[1]);

    // Signal while waiting on the abstract socket: prompt exit, name released.
    let directory = SessionDir::new(0o500);
    let mut child = spawn(&["serve", "--session-dir", directory.0.to_str().unwrap()]);
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    let name = ready["socket"].as_str().unwrap();
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let exit = finish(child, stdout);
    assert_eq!(exit.code, Some(74));
    let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    assert!(UnixStream::connect_addr(&address).is_err());
}
