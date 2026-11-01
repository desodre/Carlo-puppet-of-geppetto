//! Environment discovery without subprocesses, device mutations or feature probes.

use std::fs;
use std::io::Write;

use serde::Serialize;

use crate::error::CarloError;

/// A known capability, reported conservatively until its implementation exists.
#[derive(Debug, Serialize)]
pub struct Capability {
    name: &'static str,
    available: bool,
    reason: &'static str,
}

/// Diagnostic output schema v1. Foundation readiness does not imply feature support.
#[derive(Debug, Serialize)]
pub struct Diagnostics {
    schema_version: u32,
    agent_version: &'static str,
    platform: &'static str,
    architecture: &'static str,
    android_api: Option<u32>,
    device_abi: Option<String>,
    effective_uid: Option<u32>,
    selinux_context: Option<String>,
    /// Whether basic Android/API/ABI and shell-identity requirements hold.
    pub foundation_ready: bool,
    checks: Vec<Check>,
    capabilities: Vec<Capability>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Ok,
    Warning,
    Unavailable,
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    status: Status,
    detail: &'static str,
}

impl Diagnostics {
    /// Read Android properties and process identity without changing the device.
    ///
    /// # Errors
    /// Returns [`CarloError::InvalidEnvironment`] if Bionic returns invalid property
    /// data. Unreadable optional proc files are represented as unavailable checks.
    pub fn collect() -> Result<Self, CarloError> {
        let android_api = system_property(c"ro.build.version.sdk")?
            .map(|value| {
                value
                    .parse::<u32>()
                    .map_err(|_| CarloError::InvalidEnvironment("invalid Android API property"))
            })
            .transpose()?;
        let device_abi = system_property(c"ro.product.cpu.abi")?;
        let effective_uid = fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|text| parse_effective_uid(&text));
        let selinux_context = fs::read_to_string("/proc/self/attr/current")
            .ok()
            .map(|value| value.trim_matches(['\0', '\n', '\r']).to_owned())
            .filter(|value| !value.is_empty());
        Ok(Self::from_environment(
            std::env::consts::OS,
            std::env::consts::ARCH,
            android_api,
            device_abi,
            effective_uid,
            selinux_context,
        ))
    }

    fn from_environment(
        platform: &'static str,
        architecture: &'static str,
        android_api: Option<u32>,
        device_abi: Option<String>,
        effective_uid: Option<u32>,
        selinux_context: Option<String>,
    ) -> Self {
        let is_android = platform == "android";
        let api_ready = is_android && android_api.is_some_and(|api| api >= 29);
        let abi_ready = matches!(architecture, "x86_64" | "aarch64");
        let identity_ready = matches!(effective_uid, Some(0 | 2000));
        let checks = vec![
            check(
                "platform",
                is_android,
                "Android runtime",
                "host execution; Android foundation unavailable",
            ),
            check(
                "android_api",
                api_ready,
                "API meets the candidate native build floor (29)",
                "Android API unavailable or below candidate build floor",
            ),
            check(
                "architecture",
                abi_ready,
                "initial native build target",
                "architecture outside initial build targets",
            ),
            Check {
                name: "identity",
                status: if effective_uid == Some(0) {
                    Status::Warning
                } else if identity_ready {
                    Status::Ok
                } else {
                    Status::Unavailable
                },
                detail: if effective_uid == Some(0) {
                    "running as root; shell identity is the intended deployment"
                } else if identity_ready {
                    "running as shell"
                } else {
                    "shell identity not detected"
                },
            },
            check(
                "selinux_context",
                selinux_context.is_some(),
                "process context available; no policy changes made",
                "process context unreadable",
            ),
        ];
        Self {
            schema_version: 1,
            agent_version: env!("CARGO_PKG_VERSION"),
            platform,
            architecture,
            android_api,
            device_abi,
            effective_uid,
            selinux_context,
            foundation_ready: is_android && api_ready && abi_ready && identity_ready,
            checks,
            capabilities: vec![
                Capability {
                    name: "binder_hierarchy",
                    available: false,
                    reason: "not implemented; P1/P4",
                },
                Capability {
                    name: "display_capture",
                    available: false,
                    reason: "not implemented; P2/P6",
                },
                Capability {
                    name: "h264_stream",
                    available: false,
                    reason: "not implemented; P2/P5",
                },
                Capability {
                    name: "serve_protocol",
                    available: false,
                    reason: "not implemented; P3",
                },
            ],
        }
    }

    // P1.1 is deliberately restricted to the first reference profile and shell identity.
    pub(crate) fn api34_shell(&self) -> bool {
        self.foundation_ready && self.android_api == Some(34) && self.effective_uid == Some(2000)
    }

    /// Write a human-readable report without logging UI content or credentials.
    ///
    /// # Errors
    /// Returns [`CarloError::Io`] if writing fails.
    pub fn write_text(&self, output: &mut impl Write) -> Result<(), CarloError> {
        writeln!(output, "Carlo {} — native foundation", self.agent_version)
            .map_err(CarloError::output)?;
        writeln!(
            output,
            "platform={} architecture={} api={:?} effective_uid={:?}",
            self.platform, self.architecture, self.android_api, self.effective_uid
        )
        .map_err(CarloError::output)?;
        for check in &self.checks {
            writeln!(
                output,
                "{:?} {}: {}",
                check.status, check.name, check.detail
            )
            .map_err(CarloError::output)?;
        }
        for capability in &self.capabilities {
            writeln!(
                output,
                "unavailable {}: {}",
                capability.name, capability.reason
            )
            .map_err(CarloError::output)?;
        }
        Ok(())
    }
}

fn check(
    name: &'static str,
    available: bool,
    success: &'static str,
    failure: &'static str,
) -> Check {
    Check {
        name,
        status: if available {
            Status::Ok
        } else {
            Status::Unavailable
        },
        detail: if available { success } else { failure },
    }
}

fn parse_effective_uid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    })
}

#[cfg(not(target_os = "android"))]
fn system_property(_name: &std::ffi::CStr) -> Result<Option<String>, CarloError> {
    Ok(None)
}

#[cfg(target_os = "android")]
fn system_property(name: &std::ffi::CStr) -> Result<Option<String>, CarloError> {
    use std::ffi::{c_char, c_int};
    // Bionic's PROP_VALUE_MAX includes the terminating NUL.
    const PROP_VALUE_MAX: usize = 92;
    unsafe extern "C" {
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
    }
    let mut buffer = [0_u8; PROP_VALUE_MAX];
    // SAFETY: name is NUL-terminated, buffer has PROP_VALUE_MAX writable bytes,
    // and neither pointer outlives this synchronous Bionic call.
    let length = unsafe { __system_property_get(name.as_ptr(), buffer.as_mut_ptr().cast()) };
    let length = usize::try_from(length)
        .map_err(|_| CarloError::InvalidEnvironment("negative property length"))?;
    if length >= PROP_VALUE_MAX {
        return Err(CarloError::InvalidEnvironment(
            "property exceeds Bionic value limit",
        ));
    }
    if length == 0 {
        return Ok(None);
    }
    let value = std::str::from_utf8(&buffer[..length])
        .map_err(|_| CarloError::InvalidEnvironment("property is not UTF-8"))?;
    Ok(Some(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_uses_effective_uid_not_real_uid() {
        assert_eq!(
            parse_effective_uid("Name:\tcarlo\nUid:\t0\t2000\t0\t0\n"),
            Some(2000)
        );
        assert_eq!(parse_effective_uid("Uid:\t0\n"), None);
    }

    #[test]
    fn foundation_never_advertises_unimplemented_android_features() {
        let report = Diagnostics::from_environment(
            "android",
            "aarch64",
            Some(34),
            Some("arm64-v8a".into()),
            Some(2000),
            Some("u:r:shell:s0".into()),
        );
        assert!(report.foundation_ready);
        assert!(
            report
                .capabilities
                .iter()
                .all(|capability| !capability.available)
        );
    }

    #[test]
    fn refuses_old_api_unprivileged_uid_and_unsupported_architecture() {
        for (api, uid, arch) in [
            (Some(28), Some(2000), "aarch64"),
            (Some(34), Some(10000), "aarch64"),
            (Some(34), Some(2000), "arm"),
        ] {
            assert!(
                !Diagnostics::from_environment("android", arch, api, None, uid, None)
                    .foundation_ready
            );
        }
    }

    #[test]
    fn binder_probe_accepts_only_api34_shell_even_when_foundation_is_ready() {
        for (api, uid, expected) in [(34, 2000, true), (35, 2000, false), (34, 0, false)] {
            let report = Diagnostics::from_environment(
                "android",
                "x86_64",
                Some(api),
                None,
                Some(uid),
                None,
            );
            assert!(report.foundation_ready);
            assert_eq!(report.api34_shell(), expected);
        }
    }

    #[test]
    fn host_cannot_pass_android_readiness_even_when_running_as_root() {
        let report = Diagnostics::from_environment("linux", "x86_64", None, None, Some(0), None);
        assert!(!report.foundation_ready);
    }
}
