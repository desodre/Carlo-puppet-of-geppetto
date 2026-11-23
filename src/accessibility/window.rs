//! `IAccessibilityServiceConnection.getWindows` reply of the pinned API 36 build,
//! decoded in safe Rust. The `Region` layout and the locales encoding are
//! inferred from AOSP rather than read from the device, so every read names its
//! field and the first mismatch fails the whole list: nothing is skipped,
//! guessed or resynchronised. Titles are consumed and never kept.

use super::{Error, contract, node::Bounds};

/// Window parcel layout for a pinned profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layout {
    /// AOSP API 34: ends immediately after `mLocales` (14 fields).
    AospApi34,
    /// Samsung API 36: appends trailing `mRawType` i32 (15 fields).
    SamsungApi36,
}

impl Layout {
    pub(super) fn from_profile(profile: contract::Profile) -> Self {
        match profile {
            contract::Profile::ReferenceApi34 => Self::AospApi34,
            contract::Profile::SamsungA16xApi36 => Self::SamsungApi36,
        }
    }
}

const WINDOW_CLASS: &str = "android.view.accessibility.AccessibilityWindowInfo";
const LOCALES_CLASS: &str = "android.os.LocaleList";
const MAX_CLASS_UNITS: usize = 128;
const MAX_TITLE_BYTES: usize = 4096;
const MAX_LOCALE_BYTES: usize = 1024;
/// Rectangles of one window region; the int vector holds four ints for each.
const MAX_REGION_RECTS: usize = 256;
const MAX_CHILD_WINDOWS: usize = 256;

/// One window of the list. Holds identifiers and geometry only.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Window {
    pub(super) display_id: i32,
    /// `AccessibilityWindowInfo.TYPE_*`, 1..=7, or -1 when the WindowManager
    /// type has no accessibility mapping: the default branch of
    /// `AccessibilityWindowManager.updateWindowsLocked` (device services.jar)
    /// sets `setType(-1)` and keeps the raw type in Samsung's `mRawType`.
    pub(super) kind: i32,
    pub(super) layer: i32,
    pub(super) id: i32,
    /// Bounding box of the window's region on its display; `None` when empty.
    pub(super) bounds: Option<Bounds>,
}
// Geometry reveals the screen layout: keep it out of assertion output.
impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Window { contents: [redacted] }")
    }
}

/// Windows of one display, in the order the system wrote them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Display {
    pub(super) id: i32,
    pub(super) windows: Vec<Window>,
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

fn bad(field: &'static str, reason: &'static str) -> Error {
    Error::InvalidWindowList { field, reason }
}

impl<'a> Reader<'a> {
    fn take(&mut self, field: &'static str, len: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(len)
            .ok_or(bad(field, "offset_overflow"))?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or(bad(field, "truncated"))?;
        self.position = end;
        Ok(bytes)
    }
    fn int(&mut self, field: &'static str) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(
            self.take(field, 4)?.try_into().expect("four bytes"),
        ))
    }
    fn long(&mut self, field: &'static str) -> Result<i64, Error> {
        Ok(i64::from_le_bytes(
            self.take(field, 8)?.try_into().expect("eight bytes"),
        ))
    }
    fn count(&mut self, field: &'static str, limit: usize) -> Result<usize, Error> {
        let count = usize::try_from(self.int(field)?).map_err(|_| bad(field, "negative_length"))?;
        if count > limit {
            return Err(bad(field, "length_limit"));
        }
        Ok(count)
    }
    /// A length-prefixed, null-terminated, 4-byte padded string of `width`-byte
    /// units. Returns the units without the terminator; `None` for length -1.
    fn units(
        &mut self,
        field: &'static str,
        width: usize,
        limit: usize,
    ) -> Result<Option<&'a [u8]>, Error> {
        let len = self.int(field)?;
        if len == -1 {
            return Ok(None);
        }
        let len = usize::try_from(len).map_err(|_| bad(field, "negative_length"))?;
        if len > limit {
            return Err(bad(field, "length_limit"));
        }
        let size = (len + 1) * width;
        let bytes = self.take(field, (size + 3) & !3)?;
        if bytes[len * width..size].iter().any(|byte| *byte != 0) {
            return Err(bad(field, "string_terminator"));
        }
        Ok(Some(&bytes[..len * width]))
    }
    /// A String16 that must equal `expected` (ASCII class names only).
    fn class(
        &mut self,
        field: &'static str,
        expected: &str,
        nullable: bool,
    ) -> Result<bool, Error> {
        let Some(units) = self.units(field, 2, MAX_CLASS_UNITS)? else {
            return if nullable {
                Ok(false)
            } else {
                Err(bad(field, "missing"))
            };
        };
        let matches = units.len() == expected.len() * 2
            && units
                .chunks_exact(2)
                .zip(expected.bytes())
                .all(|(unit, byte)| unit == [byte, 0]);
        if matches {
            Ok(true)
        } else {
            Err(bad(field, "unexpected_class"))
        }
    }
    fn window(&mut self, layout: Layout) -> Result<Window, Error> {
        let display_id = self.int("display_id")?;
        let kind = self.int("type")?;
        if kind != -1 && !(1..=7).contains(&kind) {
            return Err(bad("type", "range"));
        }
        let layer = self.int("layer")?;
        self.int("boolean_properties")?;
        let id = self.int("id")?;
        self.int("parent_id")?;
        self.int("task_id")?;
        // Native Region: an int32 vector of l,t,r,b quadruples (inferred from AOSP).
        let ints = self.count("region", MAX_REGION_RECTS * 4)?;
        if !ints.is_multiple_of(4) {
            return Err(bad("region", "count_not_multiple_of_four"));
        }
        let mut bounds: Option<Bounds> = None;
        for _ in 0..ints / 4 {
            let left = self.int("region")?;
            let top = self.int("region")?;
            let right = self.int("region")?;
            let bottom = self.int("region")?;
            if left > right || top > bottom {
                return Err(bad("region", "inverted_rect"));
            }
            bounds = Some(match bounds {
                None => Bounds {
                    left,
                    top,
                    right,
                    bottom,
                },
                Some(old) => Bounds {
                    left: old.left.min(left),
                    top: old.top.min(top),
                    right: old.right.max(right),
                    bottom: old.bottom.max(bottom),
                },
            });
        }
        // Title: TextUtils CharSequence. Validated, then dropped: it is UI content.
        super::node::skip_window_title(
            self.data,
            &mut self.position,
            MAX_TITLE_BYTES,
            layout == Layout::SamsungApi36,
        )?;
        self.long("anchor_id")?;
        self.long("transition_time")?;
        let children = self.count("child_ids", MAX_CHILD_WINDOWS)?;
        self.take("child_ids", children * 4)?;
        self.int("connection_id")?;
        // writeParcelable: class name then LocaleList's String8 language tags
        // (inferred from AOSP), or a null class name when there are no locales.
        if self.class("locales", LOCALES_CLASS, true)?
            && self.units("locales", 1, MAX_LOCALE_BYTES)?.is_none()
        {
            return Err(bad("locales", "missing_tags"));
        }
        if layout == Layout::SamsungApi36 {
            self.int("raw_type")?; // Samsung-only trailing mRawType
        }
        Ok(Window {
            display_id,
            kind,
            layer,
            id,
            bounds,
        })
    }
}

/// Decode the reply after its exception header for *layout*. `Ok(None)` is the
/// null answer the system gives a session without the window capability or flag.
pub(super) fn decode_for(layout: Layout, data: &[u8]) -> Result<Option<Vec<Display>>, Error> {
    if data.len() > contract::MAX_CALLBACK_BYTES as usize || !data.len().is_multiple_of(4) {
        return Err(bad("reply", "parcel_size"));
    }
    let mut reader = Reader { data, position: 0 };
    let displays = match reader.int("presence")? {
        0 => None,
        1 => {
            let count = reader.count("display_count", contract::MAX_DISPLAYS)?;
            let mut displays = Vec::with_capacity(count);
            for _ in 0..count {
                let windows = reader.count("window_count", contract::MAX_WINDOWS_PER_DISPLAY)?;
                let mut list = Vec::with_capacity(windows);
                for _ in 0..windows {
                    reader.class("class_name", WINDOW_CLASS, false)?;
                    list.push(reader.window(layout)?);
                }
                let id = reader.int("list_display_id")?;
                if list.iter().any(|window: &Window| window.display_id != id) {
                    return Err(bad("list_display_id", "window_display_mismatch"));
                }
                if displays.iter().any(|display: &Display| display.id == id) {
                    return Err(bad("list_display_id", "duplicate"));
                }
                displays.push(Display { id, windows: list });
            }
            Some(displays)
        }
        _ => return Err(bad("presence", "boolean")),
    };
    if reader.position != data.len() {
        return Err(bad("reply", "trailing_data"));
    }
    Ok(displays)
}

/// Decode the reply after its exception header using the Samsung API 36 layout.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "API 36 alias exercised by host tests")
)]
pub(super) fn decode(data: &[u8]) -> Result<Option<Vec<Display>>, Error> {
    decode_for(Layout::SamsungApi36, data)
}

/// Synthetic reply builders shared with the traversal tests.
#[cfg(test)]
pub(super) mod fixture {
    pub(crate) fn int(value: i32) -> Vec<u8> {
        value.to_le_bytes().to_vec()
    }
    pub(crate) fn string16(value: Option<&str>) -> Vec<u8> {
        let Some(value) = value else {
            return int(-1);
        };
        let units: Vec<u16> = value.encode_utf16().collect();
        let mut data = int(units.len() as i32);
        for unit in units {
            data.extend(unit.to_le_bytes());
        }
        data.extend([0, 0]);
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
        data
    }
    pub(crate) fn string8(value: &str) -> Vec<u8> {
        let mut data = int(value.len() as i32);
        data.extend(value.as_bytes());
        data.push(0);
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
        data
    }
    /// A window as the pinned build writes it.
    #[derive(Clone)]
    pub(crate) struct Spec {
        pub(crate) display: i32,
        pub(crate) kind: i32,
        pub(crate) layer: i32,
        pub(crate) id: i32,
        pub(crate) rects: Vec<[i32; 4]>,
        pub(crate) title: Option<&'static str>,
        pub(crate) locales: Option<&'static str>,
    }
    impl Spec {
        pub(crate) fn new(display: i32, id: i32, layer: i32) -> Self {
            Self {
                display,
                kind: 1,
                layer,
                id,
                rects: vec![[0, 0, 1080, 2340]],
                title: Some("title"),
                locales: None,
            }
        }
        pub(crate) fn body(&self) -> Vec<u8> {
            self.body_for(super::Layout::SamsungApi36)
        }
        pub(crate) fn body_for(&self, layout: super::Layout) -> Vec<u8> {
            let mut data = [
                int(self.display),
                int(self.kind),
                int(self.layer),
                int(1), // ACTIVE
                int(self.id),
                int(-1),
                int(-1),
                int(self.rects.len() as i32 * 4),
            ]
            .concat();
            for rect in &self.rects {
                for value in rect {
                    data.extend(int(*value));
                }
            }
            data.extend(int(1));
            data.extend(match self.title {
                Some(title) => string8(title),
                None => int(-1),
            });
            data.extend(i64::MAX.to_le_bytes()); // anchor
            data.extend(0_i64.to_le_bytes()); // transition time
            data.extend([int(2), int(7), int(8)].concat()); // child window ids
            data.extend(int(3)); // connection id
            match self.locales {
                Some(tags) => {
                    data.extend(string16(Some("android.os.LocaleList")));
                    data.extend(string8(tags));
                }
                None => data.extend(string16(None)),
            }
            if layout == super::Layout::SamsungApi36 {
                data.extend(int(self.kind)); // mRawType
            }
            data
        }
    }
    /// Reply bytes for `(display id, windows)` lists under *layout*.
    pub(crate) fn reply_for(layout: super::Layout, displays: &[(i32, Vec<Spec>)]) -> Vec<u8> {
        let mut data = [int(1), int(displays.len() as i32)].concat();
        for (id, windows) in displays {
            data.extend(int(windows.len() as i32));
            for window in windows {
                data.extend(string16(Some(
                    "android.view.accessibility.AccessibilityWindowInfo",
                )));
                data.extend(window.body_for(layout));
            }
            data.extend(int(*id));
        }
        data
    }
    /// Reply bytes for `(display id, windows)` lists under the API 36 layout.
    pub(crate) fn reply(displays: &[(i32, Vec<Spec>)]) -> Vec<u8> {
        reply_for(super::Layout::SamsungApi36, displays)
    }
}

#[cfg(test)]
mod tests {
    use super::{fixture::*, *};

    fn field(result: Result<Option<Vec<Display>>, Error>) -> (&'static str, &'static str) {
        match result {
            Err(Error::InvalidWindowList { field, reason }) => (field, reason),
            other => panic!("expected a window list error, got {other:?}"),
        }
    }

    #[test]
    fn decodes_multiple_displays_regions_titles_and_locales() {
        let mut second = Spec::new(0, 12, 3);
        second.kind = 3;
        second.rects = vec![[0, 0, 1080, 80], [0, 2200, 1080, 2340]];
        second.title = None;
        second.locales = Some("pt-BR,en-US");
        let mut empty = Spec::new(2, 40, 0);
        empty.rects.clear();
        empty.title = Some("título 🪵");
        let data = reply(&[(0, vec![Spec::new(0, 11, 5), second]), (2, vec![empty])]);
        let displays = decode(&data).unwrap().unwrap();
        assert_eq!(displays.len(), 2);
        assert_eq!((displays[0].id, displays[1].id), (0, 2));
        let windows = &displays[0].windows;
        assert_eq!(
            (windows[0].id, windows[0].layer, windows[0].kind),
            (11, 5, 1)
        );
        assert_eq!(
            (windows[1].id, windows[1].layer, windows[1].kind),
            (12, 3, 3)
        );
        assert_eq!(
            windows[1].bounds,
            Some(Bounds {
                left: 0,
                top: 0,
                right: 1080,
                bottom: 2340
            })
        );
        assert_eq!(displays[1].windows[0].bounds, None);
        assert_eq!(displays[1].windows[0].display_id, 2);
        assert!(!format!("{displays:?}").contains("1080"));
    }

    #[test]
    fn unmapped_window_type_is_accepted_and_neighbours_are_rejected() {
        // Samsung Home's HintService window (raw type 2634) reaches the list
        // as TYPE -1 with the raw type in the trailing mRawType.
        let mut unmapped = Spec::new(0, 11, 5);
        unmapped.kind = -1;
        let displays = decode(&reply(&[(0, vec![unmapped])])).unwrap().unwrap();
        assert_eq!(displays[0].windows[0].kind, -1);
        for kind in [0, 8, -2] {
            let mut spec = Spec::new(0, 11, 5);
            spec.kind = kind;
            assert_eq!(field(decode(&reply(&[(0, vec![spec])]))), ("type", "range"));
        }
    }

    #[test]
    fn empty_lists_and_null_presence_are_distinct_valid_answers() {
        assert_eq!(decode(&int(0)).unwrap(), None);
        assert_eq!(decode(&reply(&[])).unwrap(), Some(Vec::new()));
        let displays = decode(&reply(&[(0, Vec::new())])).unwrap().unwrap();
        assert_eq!(displays.len(), 1);
        assert!(displays[0].windows.is_empty());
        assert_eq!(field(decode(&int(2))), ("presence", "boolean"));
        assert_eq!(field(decode(&[])), ("presence", "truncated"));
    }

    #[test]
    fn every_mismatch_names_its_field_and_nothing_is_resynchronised() {
        let good = reply(&[(0, vec![Spec::new(0, 11, 5)])]);
        // presence, display count, window count, then the class name
        let window = 12 + string16(Some(WINDOW_CLASS)).len();
        let patch = |offset: usize, value: i32| {
            let mut data = good.clone();
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            data
        };
        assert_eq!(
            field(decode(&patch(4, 17))),
            ("display_count", "length_limit")
        );
        assert_eq!(
            field(decode(&patch(4, -1))),
            ("display_count", "negative_length")
        );
        assert_eq!(
            field(decode(&patch(8, 65))),
            ("window_count", "length_limit")
        );
        assert_eq!(field(decode(&patch(12, -1))), ("class_name", "missing"));
        assert_eq!(
            field(decode(&patch(16, 0x42))),
            ("class_name", "unexpected_class")
        );
        assert_eq!(field(decode(&patch(window + 4, 0))), ("type", "range"));
        assert_eq!(field(decode(&patch(window + 4, 8))), ("type", "range"));
        assert_eq!(field(decode(&patch(window + 4, -2))), ("type", "range"));
        // Region: not a multiple of four, over the bound, negative, inverted.
        assert_eq!(
            field(decode(&patch(window + 28, 6))),
            ("region", "count_not_multiple_of_four")
        );
        assert_eq!(
            field(decode(&patch(window + 28, 1028))),
            ("region", "length_limit")
        );
        assert_eq!(
            field(decode(&patch(window + 28, -4))),
            ("region", "negative_length")
        );
        assert_eq!(
            field(decode(&patch(window + 32, 5000))),
            ("region", "inverted_rect")
        );
        assert_eq!(
            field(decode(&patch(window + 48, 7))),
            ("title", "char_sequence_kind")
        );
        assert_eq!(
            field(decode(&patch(window + 52, 5000))),
            ("title", "length_limit")
        );
        // The display id after the list must be the one every window carries.
        let end = good.len() - 4;
        assert_eq!(
            field(decode(&patch(end, 1))),
            ("list_display_id", "window_display_mismatch")
        );
        let mut extra = good.clone();
        extra.extend(int(0));
        assert_eq!(field(decode(&extra)), ("reply", "trailing_data"));
        let mut odd = good.clone();
        odd.push(0);
        assert_eq!(field(decode(&odd)), ("reply", "parcel_size"));
        assert_eq!(
            field(decode(&vec![0; contract::MAX_CALLBACK_BYTES as usize + 4])),
            ("reply", "parcel_size")
        );
        let duplicate = reply(&[(0, Vec::new()), (0, Vec::new())]);
        assert_eq!(field(decode(&duplicate)), ("list_display_id", "duplicate"));
    }

    #[test]
    fn spanned_titles_foreign_locale_classes_and_bad_strings_fail_typed() {
        let build = |title: Vec<u8>, locales: Vec<u8>| {
            let mut data = [int(1), int(1), int(1)].concat();
            data.extend(string16(Some(WINDOW_CLASS)));
            data.extend(
                [
                    int(0),
                    int(1),
                    int(0),
                    int(0),
                    int(5),
                    int(-1),
                    int(-1),
                    int(0),
                ]
                .concat(),
            );
            data.extend(title);
            data.extend([0_u8; 16]);
            data.extend([int(0), int(1)].concat());
            data.extend(locales);
            data.extend([int(1), int(0)].concat());
            data
        };
        let plain = [int(1), string8("t")].concat();
        assert!(decode(&build(plain.clone(), string16(None))).is_ok());
        let unspanned = [int(0), string8("t"), int(0)].concat();
        assert!(decode(&build(unspanned, string16(None))).is_ok());
        let supported_span = [
            int(0),
            string8("styled"),
            int(7),
            int(1),
            int(0),
            int(0),
            int(6),
            int(33),
            int(0),
        ]
        .concat();
        assert!(decode(&build(supported_span, string16(None))).is_ok());
        let unpinned_span = [int(0), string8("t"), int(22)].concat();
        assert_eq!(
            field(decode(&build(unpinned_span, string16(None)))),
            ("title", "char_sequence_span")
        );
        let invalid = [int(1), int(1), vec![0xff, 0, 0, 0]].concat();
        assert_eq!(
            field(decode(&build(invalid, string16(None)))),
            ("title", "utf8")
        );
        let unterminated = [int(1), int(1), vec![b'x', b'y', 0, 0]].concat();
        assert_eq!(
            field(decode(&build(unterminated, string16(None)))),
            ("title", "string_terminator")
        );
        let foreign = string16(Some("android.os.Bundle"));
        assert_eq!(
            field(decode(&build(plain.clone(), foreign))),
            ("locales", "unexpected_class")
        );
        let tagless = [string16(Some(LOCALES_CLASS)), int(-1)].concat();
        assert_eq!(
            field(decode(&build(plain.clone(), tagless))),
            ("locales", "missing_tags")
        );
        let tagged = [string16(Some(LOCALES_CLASS)), string8("pt-BR")].concat();
        assert!(decode(&build(plain, tagged)).is_ok());
    }

    #[test]
    fn every_truncation_fails_and_mutations_never_panic() {
        let mut spec = Spec::new(0, 11, 5);
        spec.locales = Some("pt-BR");
        let data = reply(&[(0, vec![spec, Spec::new(0, 12, 4)]), (3, Vec::new())]);
        assert!(decode(&data).is_ok());
        for len in (0..data.len()).step_by(4) {
            assert!(
                decode(&data[..len]).is_err(),
                "accepted truncation at {len}"
            );
        }
        for offset in 0..data.len() {
            for value in [0, 0x7f, 0xff] {
                let mut mutated = data.clone();
                mutated[offset] = value;
                let _ = decode(&mutated);
            }
        }
    }

    #[test]
    fn aosp_api34_layout_decodes_without_raw_type_and_rejects_mismatched_layouts() {
        let mut w1 = Spec::new(0, 11, 5);
        w1.kind = -1;
        w1.locales = Some("en-US");
        let mut w2 = Spec::new(2, 40, 1);
        w2.kind = 3;
        let data34 = reply_for(Layout::AospApi34, &[(0, vec![w1.clone()]), (2, vec![w2])]);
        let displays = decode_for(Layout::AospApi34, &data34).unwrap().unwrap();
        assert_eq!(displays.len(), 2);
        assert_eq!(displays[0].windows[0].kind, -1);
        assert_eq!(displays[1].windows[0].id, 40);

        // API 36 reply contains an extra mRawType int -> trailing data on API 34
        // when mRawType happens to match display_id, or list_display_id mismatch otherwise.
        let one36_trailing = reply_for(Layout::SamsungApi36, &[(1, vec![Spec::new(1, 11, 5)])]);
        assert_eq!(
            field(decode_for(Layout::AospApi34, &one36_trailing)),
            ("reply", "trailing_data")
        );

        // API 34 reply lacks mRawType -> Samsung decoder consumes list_display_id and runs short.
        let one34 = reply_for(Layout::AospApi34, &[(0, vec![w1])]);
        assert_eq!(
            field(decode_for(Layout::SamsungApi36, &one34)),
            ("list_display_id", "truncated")
        );
    }
}
