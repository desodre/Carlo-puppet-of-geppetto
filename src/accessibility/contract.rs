//! Frameworks/base 299fe6f5d6fc6f1af7c3411dcf4e5efdf7217368, Android 14 r1.

pub(super) const FINGERPRINT: &str =
    "google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys";
pub(super) const REGISTER: u32 = 10;
pub(super) const UNREGISTER: u32 = 11;
pub(super) const FIND_NODE: u32 = 3;
pub(super) const NODE_RESULT: u32 = 1;
pub(super) const ACTIVE_WINDOW: i32 = i32::MAX;
pub(super) const ROOT_NODE: i64 = -2147483650; // makeNodeId(MAX_VALUE - 1, HOST_VIEW_ID=-1)
pub(super) const INIT: u32 = 1;
pub(super) const INTERRUPT: u32 = 3;
pub(super) const CLEAR_CACHE: u32 = 5;
pub(super) const MAX_CALLBACK_BYTES: i32 = 64 * 1024;
pub(super) const DONT_SUPPRESS: i32 = 1;

pub(super) fn supported(shell34: bool, architecture: &str, fingerprint: Option<&str>) -> bool {
    shell34 && architecture == "x86_64" && fingerprint == Some(FINGERPRINT)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Int(i32),
    Long(i64),
    NullString,
}

/// Exactly AccessibilityServiceInfo.writeToParcel order. Null Java Parcelable
/// fields are null class-name Strings, not typed-object presence flags.
pub(super) const SERVICE_INFO: [Field; 22] = [
    Field::Int(0),  // eventTypes: no event payloads during bootstrap
    Field::Int(-1), // null packageNames array
    Field::Int(16), // FEEDBACK_GENERIC
    Field::Long(0), // notificationTimeout
    Field::Int(0),
    Field::Int(0),    // UI timeouts
    Field::Int(0x12), // INCLUDE_NOT_IMPORTANT_VIEWS | REPORT_VIEW_IDS
    Field::Int(0),    // crashed
    Field::NullString,
    Field::NullString,
    Field::NullString, // component/resolve/settings
    Field::Int(1),     // CAN_RETRIEVE_WINDOW_CONTENT
    Field::Int(0),
    Field::NullString, // summary
    Field::Int(0),
    Field::Int(0),
    Field::Int(0),     // description/animation/html resources
    Field::NullString, // description
    Field::Int(1),     // isAccessibilityTool
    Field::NullString, // tile service
    Field::Int(0),
    Field::Int(0), // intro resource/motion sources
];

/// The single flag ever sent to `registerUiTestAutomationService`:
/// `UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES`. Zero would make the
/// system unbind every accessibility service of the user for the session.
pub(super) const fn registration_flags() -> i32 {
    DONT_SUPPRESS
}
const _: () = assert!(registration_flags() == 1);

/// Second pinned build. Contract read from this device's own framework.jar
/// (sha256 65a982a9f223f49aea1a8ddaaa6dd90e97a73db2d5b84797b58f194949c18502) and
/// services.jar (sha256 9c07fc89884e661fdf9438d40fcfe9625cb196e5920a184a46c04718d8522606);
/// see docs/compat/samsung-a16x-api36-accessibility.json. Not executed on a device.
pub(super) const SAMSUNG_A16X_FINGERPRINT: &str =
    "samsung/a16xub/a16x:16/BP4A.251205.006/A166MUBSADZJA:user/release-keys";
const SHELL_UID: u32 = 2000;

/// `IAccessibilityServiceConnection.getWindows` (API 36 profile only).
pub(super) const GET_WINDOWS: u32 = 10;
/// `IAccessibilityInteractionConnectionCallback.setFindAccessibilityNodeInfosResult`.
pub(super) const NODE_RESULTS: u32 = 2;
/// `…setPrefetchAccessibilityNodeInfoResult`; tolerated and dropped.
pub(super) const PREFETCH_RESULT: u32 = 3;
/// `IAccessibilityServiceClient.onSystemActionsChanged` (API 36 profile only).
pub(super) const SYSTEM_ACTIONS_CHANGED: u32 = 16;
/// Interaction id an app answers with (and a null node) when the window is invalid.
pub(super) const WINDOW_GONE_INTERACTION: i32 = i32::MAX;
/// `AccessibilityNodeInfo.FLAG_PREFETCH_DESCENDANTS_DEPTH_FIRST`.
pub(super) const PREFETCH_DESCENDANTS_DEPTH_FIRST: i32 = 8;
/// `AccessibilityNodeInfo.FLAG_PREFETCH_UNINTERRUPTIBLE`: one code-2 answer that
/// carries the requested node and its prefetched nodes.
pub(super) const PREFETCH_UNINTERRUPTIBLE: i32 = 32;
/// `AccessibilityNodeInfo.MAX_NUMBER_OF_PREFETCHED_NODES` plus the requested node,
/// rounded up; a longer list is refused before any element is read.
pub(super) const MAX_BATCH_NODES: usize = 64;
/// Byte bound of one API 36 result callback. Callbacks are oneway transactions
/// and the kernel gives a process at most half of its ~1 MiB Binder buffer for
/// asynchronous data, so no deliverable callback is larger than this; a 51-node
/// batch is typically 20-100 KiB. The API 34 profile keeps [`MAX_CALLBACK_BYTES`].
pub(super) const MAX_BATCH_BYTES: i32 = 512 * 1024;
pub(super) const MAX_DISPLAYS: usize = 16;
pub(super) const MAX_WINDOWS_PER_DISPLAY: usize = 64;

/// A build whose private contract is pinned. Selected by exact fingerprint, API,
/// architecture and shell identity; nothing is inferred from `SDK_INT` alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Profile {
    /// Android 14 r1 x86_64 emulator: session and single-node proofs only.
    ReferenceApi34,
    /// Samsung Galaxy A16 5G, Android 16: session, window list and hierarchy.
    SamsungA16xApi36,
}

impl Profile {
    /// `shell_api` is only `Some` when the foundation checks passed for the shell UID.
    pub(super) fn select(
        shell_api: Option<u32>,
        effective_uid: Option<u32>,
        architecture: &str,
        fingerprint: Option<&str>,
    ) -> Option<Self> {
        if effective_uid != Some(SHELL_UID) {
            return None;
        }
        match (shell_api, architecture, fingerprint) {
            (Some(34), "x86_64", Some(FINGERPRINT)) => Some(Self::ReferenceApi34),
            (Some(36), "aarch64", Some(SAMSUNG_A16X_FINGERPRINT)) => Some(Self::SamsungA16xApi36),
            _ => None,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::ReferenceApi34 => "reference_api34",
            Self::SamsungA16xApi36 => "samsung_a16x_api36",
        }
    }

    /// The window-enumeration and batch contracts are pinned for this profile.
    pub(super) fn hierarchy(self) -> bool {
        matches!(self, Self::SamsungA16xApi36)
    }

    /// `AccessibilityServiceInfo` sent at registration.
    pub(super) fn service_info(self) -> &'static [Field] {
        match self {
            Self::ReferenceApi34 => &SERVICE_INFO,
            Self::SamsungA16xApi36 => &SERVICE_INFO_API36,
        }
    }
}

/// The 22 API 34 fields in the same order plus the trailing
/// `observedMotionEventSources`. Flags add `RETRIEVE_INTERACTIVE_WINDOWS` (64):
/// without it and capability 1 `getWindows` answers null.
pub(super) const SERVICE_INFO_API36: [Field; 23] = [
    Field::Int(0),  // eventTypes: no event payloads
    Field::Int(-1), // null packageNames array
    Field::Int(16), // FEEDBACK_GENERIC
    Field::Long(0), // notificationTimeout
    Field::Int(0),
    Field::Int(0),    // UI timeouts
    Field::Int(0x52), // INCLUDE_NOT_IMPORTANT_VIEWS | REPORT_VIEW_IDS | RETRIEVE_INTERACTIVE_WINDOWS
    Field::Int(0),    // crashed
    Field::NullString,
    Field::NullString,
    Field::NullString, // component/resolve/settings
    Field::Int(1),     // CAN_RETRIEVE_WINDOW_CONTENT
    Field::Int(0),
    Field::NullString, // summary
    Field::Int(0),
    Field::Int(0),
    Field::Int(0),     // description/animation/html resources
    Field::NullString, // description
    Field::Int(1),     // isAccessibilityTool
    Field::NullString, // tile service
    Field::Int(0),
    Field::Int(0), // intro resource/motion sources
    Field::Int(0), // observedMotionEventSources
];

#[cfg(test)]
mod profile_tests {
    use super::*;
    fn bytes(fields: &[Field]) -> Vec<u8> {
        fields
            .iter()
            .flat_map(|field| match field {
                Field::Int(v) => v.to_le_bytes().to_vec(),
                Field::Long(v) => v.to_le_bytes().to_vec(),
                Field::NullString => (-1_i32).to_le_bytes().to_vec(),
            })
            .collect()
    }
    #[test]
    fn profiles_are_selected_by_exact_fingerprint_api_architecture_and_shell_uid() {
        let samsung = Some(SAMSUNG_A16X_FINGERPRINT);
        assert_eq!(
            Profile::select(Some(34), Some(2000), "x86_64", Some(FINGERPRINT)),
            Some(Profile::ReferenceApi34)
        );
        assert_eq!(
            Profile::select(Some(36), Some(2000), "aarch64", samsung),
            Some(Profile::SamsungA16xApi36)
        );
        let userdebug =
            SAMSUNG_A16X_FINGERPRINT.replace("user/release-keys", "userdebug/test-keys");
        for (api, uid, arch, fingerprint) in [
            (Some(36), Some(2000), "aarch64", Some(FINGERPRINT)),
            (Some(34), Some(2000), "x86_64", samsung),
            (Some(35), Some(2000), "aarch64", samsung),
            (Some(37), Some(2000), "aarch64", samsung),
            (None, Some(2000), "aarch64", samsung),
            (Some(36), Some(0), "aarch64", samsung),
            (Some(36), Some(1000), "aarch64", samsung),
            (Some(36), None, "aarch64", samsung),
            (Some(36), Some(2000), "x86_64", samsung),
            (Some(36), Some(2000), "aarch64", Some(userdebug.as_str())),
            (Some(36), Some(2000), "aarch64", Some("")),
            (Some(36), Some(2000), "aarch64", None),
            (Some(34), Some(0), "x86_64", Some(FINGERPRINT)),
            (Some(34), Some(2000), "aarch64", Some(FINGERPRINT)),
        ] {
            assert_eq!(Profile::select(api, uid, arch, fingerprint), None);
        }
        assert!(!Profile::ReferenceApi34.hierarchy());
        assert!(Profile::SamsungA16xApi36.hierarchy());
        assert_eq!(Profile::ReferenceApi34.name(), "reference_api34");
        assert_eq!(Profile::SamsungA16xApi36.name(), "samsung_a16x_api36");
    }
    #[test]
    fn recorded_contract_matches_the_code_and_names_its_device_evidence() {
        let text = include_str!("../../docs/compat/samsung-a16x-api36-accessibility.json");
        let doc: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(doc["executed_on_device"], true);
        assert_eq!(doc["evidence"], "docs/validation/p4-hierarchy-core.md");
        assert_eq!(doc["profile"]["fingerprint"], SAMSUNG_A16X_FINGERPRINT);
        assert_eq!(doc["profile"]["android_api"], 36);
        assert_eq!(doc["manager"]["register"]["transaction"], REGISTER);
        assert_eq!(doc["manager"]["unregister"]["transaction"], UNREGISTER);
        assert_eq!(
            doc["manager"]["register"]["registration_flags_must_equal"],
            1
        );
        assert_eq!(doc["connection"]["find_node"]["transaction"], FIND_NODE);
        assert_eq!(doc["connection"]["get_windows"]["transaction"], GET_WINDOWS);
        assert_eq!(doc["callback"]["max_parcel_bytes"], MAX_BATCH_BYTES);
        assert_eq!(
            doc["callback"]["invalid_window_sentinel"]["interaction_id"],
            i32::MAX
        );
        assert_eq!(doc["node_info"]["fields"].as_array().unwrap().len(), 50);
        let fields = doc["service_info"]["fields"].as_array().unwrap();
        assert_eq!(fields.len(), SERVICE_INFO_API36.len());
        for (recorded, field) in fields.iter().zip(SERVICE_INFO_API36) {
            match field {
                Field::Int(value) => assert_eq!(recorded["value"], value),
                Field::Long(value) => assert_eq!(recorded["value"], value),
                Field::NullString => assert!(recorded["value"].is_null()),
            }
        }
    }
    #[test]
    fn registration_never_suppresses_the_users_accessibility_services() {
        assert_eq!(registration_flags(), 1);
        assert_eq!(DONT_SUPPRESS, 1);
    }
    #[test]
    fn api36_service_info_is_the_api34_layout_plus_one_trailing_int() {
        let old = bytes(&SERVICE_INFO);
        let new = bytes(&SERVICE_INFO_API36);
        assert_eq!(Profile::ReferenceApi34.service_info(), &SERVICE_INFO[..]);
        assert_eq!(
            Profile::SamsungA16xApi36.service_info(),
            &SERVICE_INFO_API36[..]
        );
        assert_eq!(new.len(), 96);
        assert_eq!(new[..28], old[..28]);
        assert_eq!(i32::from_le_bytes(new[28..32].try_into().unwrap()), 0x52);
        assert_eq!(new[32..92], old[32..92]);
        assert_eq!(new[92..], [0; 4]);
        let int = |offset: usize| i32::from_le_bytes(new[offset..offset + 4].try_into().unwrap());
        assert_eq!((int(0), int(4), int(8)), (0, -1, 16));
        assert_eq!((int(48), int(76)), (1, 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_calls_require_exact_reference_profile() {
        assert!(supported(true, "x86_64", Some(FINGERPRINT)));
        for (shell, arch, fp) in [
            (false, "x86_64", Some(FINGERPRINT)),
            (true, "aarch64", Some(FINGERPRINT)),
            (true, "x86_64", Some("other")),
            (true, "x86_64", None),
        ] {
            assert!(!supported(shell, arch, fp));
        }
    }
    #[test]
    fn minimal_info_has_fixed_field_order_and_size() {
        let bytes: Vec<u8> = SERVICE_INFO
            .iter()
            .flat_map(|field| match field {
                Field::Int(v) => v.to_le_bytes().to_vec(),
                Field::Long(v) => v.to_le_bytes().to_vec(),
                Field::NullString => (-1_i32).to_le_bytes().to_vec(),
            })
            .collect();
        // Native Binder object offsets are not part of this pure data fixture.
        assert_eq!(bytes.len(), 92);
        assert_eq!(i32::from_le_bytes(bytes[28..32].try_into().unwrap()), 0x12);
        assert_eq!(i32::from_le_bytes(bytes[48..52].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(bytes[76..80].try_into().unwrap()), 1);
    }
}
