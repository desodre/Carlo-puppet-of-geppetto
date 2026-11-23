//! Transport-independent control channel: one request frame in, one response out.
//!
//! Responses are built only from constants and this build's own data. Nothing a
//! client sends is ever copied into a response or into a diagnostic. The video
//! commands are delegated to [`Video`], which owns the second channel;
//! `hierarchy.dump` to a [`HierarchySource`], whose document is the only
//! response that is not a control frame.

use serde_json::{Value, json};

use super::hierarchy::{self, HierarchySource};
use super::socket::ControlSocket;
use super::video::Video;
use crate::diagnostics::Diagnostics;
use crate::protocol::{Frame, Kind, VERSION};

/// Commands reserved by the plan that have no implementation behind them yet.
const RESERVED: [&str; 2] = ["display.info", "stats"];

/// What the connection owner must do after sending a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Next {
    /// Keep reading requests.
    Continue,
    /// `shutdown` was acknowledged: close and exit successfully.
    Shutdown,
    /// The handshake failed with this stable code: close and fail.
    Reject(&'static str),
}

/// A response payload of `kind` for the request ID it answers. A hierarchy
/// payload is screen contents: deliberately not `Debug`.
pub(super) struct Reply {
    pub kind: Kind,
    pub request_id: u32,
    pub payload: Vec<u8>,
    pub next: Next,
}

/// A response body before encoding.
enum Body {
    Json(Value),
    Hierarchy(Vec<u8>),
}

/// Handshake state for the single accepted client. Deliberately not `Debug`:
/// it holds the session identifier.
pub(super) struct Control {
    session: String,
    hello: Value,
    capabilities: Value,
    initialized: bool,
    video: Video,
    hierarchy: Box<dyn HierarchySource>,
}

impl Control {
    /// `session` is the identifier printed on the ready line; identity and
    /// capabilities come from the same [`Diagnostics`] that `doctor` reports.
    pub(super) fn new(
        session: String,
        diagnostics: &Diagnostics,
        video: Video,
        hierarchy: Box<dyn HierarchySource>,
    ) -> Self {
        let capabilities =
            serde_json::to_value(diagnostics.capabilities()).unwrap_or_else(|_| json!([]));
        let hello = json!({
            "ok": true,
            "protocol": VERSION,
            "agent": { "name": "carlo", "version": env!("CARGO_PKG_VERSION") },
            "device": {
                "android_api": diagnostics.android_api(),
                "abi": diagnostics.device_abi(),
                "architecture": std::env::consts::ARCH,
            },
            "capabilities": capabilities,
            "limits": {
                "control_bytes": Kind::Control.max_payload(),
                "payload_bytes": Kind::Hierarchy.max_payload(),
            },
        });
        Self {
            session,
            hello,
            capabilities,
            initialized: false,
            video,
            hierarchy,
        }
    }

    /// Hand the session's listener to the video channel, which admits one
    /// attachment bound to this session within its deadline and then closes it.
    pub(super) fn listen(&mut self, listener: ControlSocket) {
        self.video.listen(listener, self.session.clone());
    }

    /// Stop any stream, close the video channel and the listener, release the
    /// hierarchy source. Bounded.
    pub(super) fn teardown(&mut self) {
        self.video.shutdown();
        self.hierarchy.teardown();
    }

    /// Whether a `hello` has succeeded on this connection.
    pub(super) fn initialized(&self) -> bool {
        self.initialized
    }

    /// Answer one frame. Never panics on client data and never echoes it.
    pub(super) fn handle(&mut self, frame: &Frame) -> Reply {
        let (kind, payload, next) = match self.respond(frame) {
            (Body::Json(body), next) => (Kind::Control, body.to_string().into_bytes(), next),
            (Body::Hierarchy(document), next) => (Kind::Hierarchy, document, next),
        };
        Reply {
            kind,
            request_id: frame.request_id,
            payload,
            next,
        }
    }

    fn respond(&mut self, frame: &Frame) -> (Body, Next) {
        if frame.kind != Kind::Control {
            return (Body::Json(failure("unexpected_kind")), Next::Continue);
        }
        // Non-UTF-8 and over-nested input fail here; serde_json bounds recursion.
        let request = serde_json::from_slice::<Value>(&frame.payload).ok();
        let Some(command) = request
            .as_ref()
            .and_then(|value| value.as_object())
            .and_then(|object| object.get("command"))
            .and_then(Value::as_str)
        else {
            return (Body::Json(failure("invalid_request")), Next::Continue);
        };
        let request = request.as_ref().unwrap_or(&Value::Null);
        let (body, next) = if !self.initialized {
            self.handshake(command, request)
        } else if command == "hierarchy.dump" {
            return (self.dump(), Next::Continue);
        } else {
            self.command(frame.request_id, command, request)
        };
        (Body::Json(body), next)
    }

    /// One snapshot: the document, or a control error with a stable code.
    fn dump(&mut self) -> Body {
        match hierarchy::answer(self.hierarchy.dump()) {
            hierarchy::Answer::Document(document) => Body::Hierarchy(document),
            hierarchy::Answer::Failure(code) => Body::Json(failure(code)),
        }
    }

    fn command(&mut self, request_id: u32, command: &str, request: &Value) -> (Value, Next) {
        match command {
            "hello" => (failure("already_initialized"), Next::Continue),
            "ping" => (json!({ "ok": true }), Next::Continue),
            "capabilities" => (
                json!({ "ok": true, "capabilities": self.capabilities }),
                Next::Continue,
            ),
            "shutdown" => (json!({ "ok": true }), Next::Shutdown),
            "channels" => (
                json!({ "ok": true, "video": self.video.channel() }),
                Next::Continue,
            ),
            "video.start" => (self.video.start(request_id, request), Next::Continue),
            "video.stop" => (self.video.stop(), Next::Continue),
            name if RESERVED.contains(&name) => (failure("unavailable"), Next::Continue),
            _ => (failure("unknown_command"), Next::Continue),
        }
    }

    fn handshake(&mut self, command: &str, request: &Value) -> (Value, Next) {
        if command != "hello" {
            return reject("handshake_required");
        }
        // A missing or non-integer version is as unsupported as a different one.
        if request["protocol"].as_u64() != Some(u64::from(VERSION)) {
            let body = json!({
                "ok": false,
                "error": { "code": "unsupported_protocol", "supported": [VERSION] },
            });
            return (body, Next::Reject("unsupported_protocol"));
        }
        let presented = request["session"].as_str().unwrap_or("");
        if !constant_time_eq(presented.as_bytes(), self.session.as_bytes()) {
            return reject("invalid_session");
        }
        self.initialized = true;
        (self.hello.clone(), Next::Continue)
    }
}

pub(super) fn failure(code: &'static str) -> Value {
    json!({ "ok": false, "error": { "code": code } })
}

fn reject(code: &'static str) -> (Value, Next) {
    (failure(code), Next::Reject(code))
}

/// Compare without an early exit on the first differing byte. Only the length
/// (fixed and public: 32 hex digits) is observable through timing.
pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let difference = left
        .iter()
        .zip(right)
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b));
    std::hint::black_box(difference) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::accessibility::Error;
    use crate::serve::hierarchy::fake::Fake;

    const SESSION: &str = "00112233445566778899aabbccddeeff";
    const SENTINEL: &str = "CLIENT_SUPPLIED_SENTINEL";

    fn control() -> Control {
        with_source(Fake::default())
    }

    fn with_source(source: Fake) -> Control {
        let diagnostics = Diagnostics::from_environment(
            "android",
            "x86_64",
            Some(34),
            Some("x86_64".into()),
            Some(2000),
            None,
        );
        Control::new(
            SESSION.into(),
            &diagnostics,
            Video::unattached(),
            Box::new(source),
        )
    }

    fn initialized_with(source: Fake) -> Control {
        let mut control = with_source(source);
        let (body, next, _) = send(&mut control, 1, &hello(SESSION));
        assert_eq!((body["ok"].as_bool(), next), (Some(true), Next::Continue));
        control
    }

    fn dump(control: &mut Control, request_id: u32) -> Reply {
        let request = json!({ "command": "hierarchy.dump", "argument": SENTINEL }).to_string();
        let reply = control.handle(&Frame {
            kind: Kind::Control,
            request_id,
            payload: request.into_bytes(),
        });
        assert_eq!((reply.request_id, reply.next), (request_id, Next::Continue));
        reply
    }

    fn control_code(reply: &Reply) -> String {
        assert_eq!(reply.kind, Kind::Control);
        let text = std::str::from_utf8(&reply.payload).unwrap();
        // Neither the client's strings nor any UI content reach an error.
        assert!(!text.contains(SENTINEL) && !text.contains(SESSION));
        assert!(!text.contains('<'));
        let body: Value = serde_json::from_str(text).unwrap();
        code(&body).to_owned()
    }

    fn send(control: &mut Control, request_id: u32, payload: &[u8]) -> (Value, Next, String) {
        let reply = control.handle(&Frame {
            kind: Kind::Control,
            request_id,
            payload: payload.to_vec(),
        });
        assert_eq!((reply.kind, reply.request_id), (Kind::Control, request_id));
        let text = String::from_utf8(reply.payload).unwrap();
        assert!(!text.contains(SENTINEL) && !text.contains(SESSION));
        (serde_json::from_str(&text).unwrap(), reply.next, text)
    }

    fn hello(session: &str) -> Vec<u8> {
        json!({ "command": "hello", "protocol": 1, "session": session })
            .to_string()
            .into_bytes()
    }

    fn initialized() -> Control {
        let mut control = control();
        let (body, next, _) = send(&mut control, 1, &hello(SESSION));
        assert_eq!((body["ok"].as_bool(), next), (Some(true), Next::Continue));
        assert!(control.initialized());
        control
    }

    fn code(body: &Value) -> &str {
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"].as_object().unwrap().len(), 1);
        body["error"]["code"].as_str().unwrap()
    }

    #[test]
    fn hello_reports_identity_limits_and_the_doctor_capability_list() {
        let mut control = control();
        let (body, next, _) = send(&mut control, 0xDEAD_BEEF, &hello(SESSION));
        assert_eq!(next, Next::Continue);
        assert_eq!(
            body,
            json!({
                "ok": true,
                "protocol": 1,
                "agent": { "name": "carlo", "version": env!("CARGO_PKG_VERSION") },
                "device": {
                    "android_api": 34,
                    "abi": "x86_64",
                    "architecture": std::env::consts::ARCH,
                },
                "capabilities": body["capabilities"],
                "limits": { "control_bytes": 65536, "payload_bytes": 8_388_608 },
            })
        );
        let capabilities = body["capabilities"].as_array().unwrap();
        assert_eq!(capabilities.len(), 4);
        for capability in capabilities {
            assert!(capability["name"].is_string() && capability["reason"].is_string());
            // The API 34 reference profile is not the mirror's pinned build.
            assert_eq!(
                capability["available"],
                capability["name"] == "serve_protocol"
            );
        }
        let host = Diagnostics::from_environment("linux", "x86_64", None, None, Some(1000), None);
        let mut control = Control::new(
            SESSION.into(),
            &host,
            Video::unattached(),
            Box::new(Fake::default()),
        );
        let (body, _, _) = send(&mut control, 1, &hello(SESSION));
        assert!(body["device"]["android_api"].is_null() && body["device"]["abi"].is_null());
    }

    #[test]
    fn first_request_must_be_a_valid_hello_or_the_connection_is_rejected() {
        for command in ["ping", "capabilities", "shutdown", "stats", SENTINEL] {
            let mut control = control();
            let request = json!({ "command": command, "session": SESSION }).to_string();
            let (body, next, _) = send(&mut control, 3, request.as_bytes());
            assert_eq!(code(&body), "handshake_required");
            assert_eq!(next, Next::Reject("handshake_required"));
            assert!(!control.initialized());
        }
        for protocol in [
            json!(2),
            json!(0),
            json!("1"),
            json!(1.5),
            json!(-1),
            Value::Null,
        ] {
            let mut control = control();
            let request =
                json!({ "command": "hello", "protocol": protocol, "session": SESSION }).to_string();
            let (body, next, _) = send(&mut control, 4, request.as_bytes());
            assert_eq!(
                body,
                json!({"ok": false,
                       "error": {"code": "unsupported_protocol", "supported": [1]}})
            );
            assert_eq!(next, Next::Reject("unsupported_protocol"));
            assert!(!control.initialized());
        }
        let mut control = control();
        let (body, next, _) = send(&mut control, 4, br#"{"command":"hello","session":"x"}"#);
        assert_eq!(body["error"]["code"], "unsupported_protocol");
        assert_eq!(next, Next::Reject("unsupported_protocol"));
    }

    #[test]
    fn wrong_missing_or_mistyped_session_is_rejected() {
        let mut flipped = SESSION.to_owned().into_bytes();
        *flipped.last_mut().unwrap() = b'0';
        let flipped = String::from_utf8(flipped).unwrap();
        for session in [
            json!(flipped),
            json!(SESSION.to_uppercase()),
            json!(&SESSION[..31]),
            json!(format!("{SESSION}0")),
            json!(""),
            json!(SENTINEL),
            json!(7),
            json!([SESSION]),
            Value::Null,
        ] {
            let mut control = control();
            let request =
                json!({ "command": "hello", "protocol": 1, "session": session }).to_string();
            let (body, next, _) = send(&mut control, 5, request.as_bytes());
            assert_eq!(code(&body), "invalid_session");
            assert_eq!(next, Next::Reject("invalid_session"));
            assert!(!control.initialized());
        }
        let mut control = control();
        let (body, next, _) = send(&mut control, 5, br#"{"command":"hello","protocol":1}"#);
        assert_eq!(code(&body), "invalid_session");
        assert_eq!(next, Next::Reject("invalid_session"));
        assert!(constant_time_eq(b"abc", b"abc") && !constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab") && constant_time_eq(b"", b""));
    }

    #[test]
    fn commands_after_the_handshake_follow_the_contract_and_keep_the_connection() {
        let mut control = initialized();
        let (body, next, _) = send(&mut control, 2, &hello(SESSION));
        assert_eq!((code(&body), next), ("already_initialized", Next::Continue));
        let (body, next, _) = send(&mut control, 2, &hello(SENTINEL));
        assert_eq!((code(&body), next), ("already_initialized", Next::Continue));
        let (body, next, _) = send(&mut control, 3, br#"{"command":"ping","extra":[1,2]}"#);
        assert_eq!((body, next), (json!({"ok": true}), Next::Continue));
        let (body, next, _) = send(&mut control, 4, br#"{"command":"capabilities"}"#);
        assert_eq!(next, Next::Continue);
        assert_eq!(body.as_object().unwrap().len(), 2);
        assert_eq!(body["ok"], true);
        assert_eq!(body["capabilities"].as_array().unwrap().len(), 4);
        for name in RESERVED {
            let request = json!({ "command": name, "argument": SENTINEL }).to_string();
            let (body, next, _) = send(&mut control, 5, request.as_bytes());
            assert_eq!((code(&body), next), ("unavailable", Next::Continue));
        }
        // Without a video channel the video commands answer from their own table.
        let (body, next, _) = send(&mut control, 5, br#"{"command":"channels"}"#);
        assert_eq!(
            (body, next),
            (json!({"ok": true, "video": "absent"}), Next::Continue)
        );
        let request = json!({ "command": "video.start", "argument": SENTINEL }).to_string();
        let (body, next, _) = send(&mut control, 5, request.as_bytes());
        assert_eq!(
            (code(&body), next),
            ("video_channel_absent", Next::Continue)
        );
        let (body, next, _) = send(&mut control, 5, br#"{"command":"video.stop"}"#);
        assert_eq!((code(&body), next), ("not_streaming", Next::Continue));
        for name in [
            SENTINEL,
            "",
            "PING",
            "ping ",
            "hierarchy",
            "video",
            "attach",
        ] {
            let request = json!({ "command": name }).to_string();
            let (body, next, _) = send(&mut control, 6, request.as_bytes());
            assert_eq!((code(&body), next), ("unknown_command", Next::Continue));
        }
        let (body, next, _) = send(&mut control, 7, br#"{"command":"shutdown"}"#);
        assert_eq!((body, next), (json!({"ok": true}), Next::Shutdown));
    }

    #[test]
    fn malformed_requests_and_other_kinds_are_refused_without_echo_in_any_state() {
        let nested = "[".repeat(4096);
        let payloads: [&[u8]; 12] = [
            b"",
            b"{",
            b"null",
            b"[]",
            b"\"hello\"",
            b"42",
            b"{}",
            br#"{"command":7}"#,
            br#"{"command":["hello"]}"#,
            br#"{"Command":"hello","CLIENT_SUPPLIED_SENTINEL":1}"#,
            b"\xff\xfe{\"command\":\"ping\"}",
            nested.as_bytes(),
        ];
        for mut control in [control(), initialized()] {
            let before = control.initialized();
            for payload in payloads {
                let (body, next, _) = send(&mut control, 9, payload);
                assert_eq!((code(&body), next), ("invalid_request", Next::Continue));
            }
            for kind in [Kind::Hierarchy, Kind::Video] {
                let reply = control.handle(&Frame {
                    kind,
                    request_id: 77,
                    payload: hello(SESSION),
                });
                assert_eq!((reply.request_id, reply.next), (77, Next::Continue));
                let body: Value = serde_json::from_slice(&reply.payload).unwrap();
                assert_eq!(code(&body), "unexpected_kind");
            }
            assert_eq!(control.initialized(), before);
        }
    }

    #[test]
    fn every_response_fits_a_control_frame() {
        let mut control = control();
        let limit = Kind::Control.max_payload() as usize;
        assert!(control.handle(&frame(&hello(SESSION))).payload.len() < limit / 8);
        assert!(
            control
                .handle(&frame(br#"{"command":"capabilities"}"#))
                .payload
                .len()
                < limit / 8
        );
    }

    #[test]
    fn hierarchy_dump_answers_raw_xml_on_a_hierarchy_frame_with_the_request_id() {
        let xml = "<?xml version='1.0' encoding='UTF-8'?><hierarchy rotation=\"0\"/>";
        let mut source = Fake::default();
        source.results.push_back(Ok(xml.as_bytes().to_vec()));
        source.results.push_back(Ok(b"<hierarchy/>".to_vec()));
        let dumps = source.dumps.clone();
        let mut control = initialized_with(source);
        let reply = dump(&mut control, 0xFEED_F00D);
        assert_eq!(reply.kind, Kind::Hierarchy);
        // Raw bytes, never JSON or Base64.
        assert_eq!(reply.payload, xml.as_bytes());
        let reply = dump(&mut control, 2);
        assert_eq!(reply.kind, Kind::Hierarchy);
        assert_eq!(reply.payload, b"<hierarchy/>");
        assert_eq!(dumps.get(), 2);
        // The connection keeps working after a document.
        let (body, next, _) = send(&mut control, 3, br#"{"command":"ping"}"#);
        assert_eq!((body, next), (json!({"ok": true}), Next::Continue));
    }

    #[test]
    fn hierarchy_dump_failures_are_control_errors_with_stable_codes() {
        let cases = [
            (Error::UnsupportedProfile, "unavailable"),
            (Error::Conflict, "conflict"),
            (Error::WindowsUnavailable, "windows_unavailable"),
            (
                Error::InconsistentSnapshot {
                    reason: "window_changed",
                    attempts: 3,
                },
                "inconsistent_snapshot",
            ),
            (Error::Truncated { limit: "xml_bytes" }, "truncated"),
            (
                Error::UnsupportedNode {
                    class: "text",
                    count: 1,
                },
                "unsupported_node",
            ),
            (Error::InitializationTimeout, "timeout"),
            (Error::QueryTimeout, "timeout"),
            (
                Error::RemoteException {
                    operation: "get_windows",
                    code: -3,
                },
                "internal",
            ),
            (Error::Runtime("snapshot_sequence_poisoned"), "internal"),
        ];
        let expected: Vec<_> = cases.iter().map(|(_, code)| *code).collect();
        let source = Fake {
            results: cases.into_iter().map(|(error, _)| Err(error)).collect(),
            ..Fake::default()
        };
        let mut control = initialized_with(source);
        for (id, expected) in (10..).zip(expected) {
            assert_eq!(control_code(&dump(&mut control, id)), expected);
        }
        // A document that cannot be sent is refused, not cut.
        let too_large = vec![b'a'; Kind::Hierarchy.max_payload() as usize + 1];
        let mut control = initialized_with(Fake {
            results: [Ok(too_large), Ok(b"<a>\xff</a>".to_vec())].into(),
            ..Fake::default()
        });
        assert_eq!(control_code(&dump(&mut control, 1)), "truncated");
        assert_eq!(control_code(&dump(&mut control, 2)), "internal");
    }

    #[test]
    fn hierarchy_dump_needs_the_handshake_and_a_control_frame() {
        let source = Fake::default();
        let dumps = source.dumps.clone();
        let mut control = with_source(source);
        let reply = control.handle(&Frame {
            kind: Kind::Control,
            request_id: 4,
            payload: br#"{"command":"hierarchy.dump"}"#.to_vec(),
        });
        assert_eq!(
            (reply.request_id, reply.next, reply.kind),
            (4, Next::Reject("handshake_required"), Kind::Control)
        );
        let body: Value = serde_json::from_slice(&reply.payload).unwrap();
        assert_eq!(code(&body), "handshake_required");
        assert!(!control.initialized());
        assert_eq!(dumps.get(), 0);
        let mut control = initialized_with(Fake {
            dumps: dumps.clone(),
            ..Fake::default()
        });
        let reply = control.handle(&Frame {
            kind: Kind::Hierarchy,
            request_id: 5,
            payload: br#"{"command":"hierarchy.dump"}"#.to_vec(),
        });
        assert_eq!(reply.kind, Kind::Control);
        let body: Value = serde_json::from_slice(&reply.payload).unwrap();
        assert_eq!(code(&body), "unexpected_kind");
        assert_eq!(dumps.get(), 0);
    }

    #[test]
    fn teardown_releases_the_hierarchy_source_on_every_exit() {
        // Shutdown, a rejected handshake and an abnormal end all reach teardown,
        // which `serve::run` calls once whatever ended the conversation.
        for request in [
            br#"{"command":"shutdown"}"#.to_vec(),
            hello(SENTINEL),
            b"{".to_vec(),
        ] {
            let source = Fake::default();
            let teardowns = source.teardowns.clone();
            let mut control = if request == hello(SENTINEL) {
                with_source(source)
            } else {
                initialized_with(source)
            };
            let _ = control.handle(&frame(&request));
            assert_eq!(teardowns.get(), 0);
            control.teardown();
            assert_eq!(teardowns.get(), 1);
        }
    }

    fn frame(payload: &[u8]) -> Frame {
        Frame {
            kind: Kind::Control,
            request_id: 1,
            payload: payload.to_vec(),
        }
    }
}
