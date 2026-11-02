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
