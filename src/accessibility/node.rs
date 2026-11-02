//! Android 14 r1 AccessibilityNodeInfo/TextUtils codec. No FFI or unchecked reads.
//! Unknown layouts fail closed; decoded values are command data, never diagnostics.
use super::{Error, contract, query::Envelope};
use serde::Serialize;

const MAX_STRING_UNITS: usize = 8192;
const MAX_CHILDREN: usize = 1024;
const MAX_COLLECTION: usize = 128;
const UNDEFINED_NODE: i64 = 0x7fff_ffff_7fff_ffff;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(super) struct Bounds {
    pub(super) left: i32,
    pub(super) top: i32,
    pub(super) right: i32,
    pub(super) bottom: i32,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(super) struct Node {
    pub(super) source_id: i64,
    pub(super) window_id: i32,
    pub(super) parent_id: i64,
    pub(super) connection_id: i32,
    pub(super) children: Vec<i64>,
    pub(super) bounds_in_parent: Bounds,
    pub(super) bounds_in_screen: Bounds,
    pub(super) bounds_in_window: Bounds,
    pub(super) package: Option<String>,
    pub(super) class: Option<String>,
    pub(super) text: Option<String>,
    pub(super) content_description: Option<String>,
    pub(super) resource_id: Option<String>,
    pub(super) unique_id: Option<String>,
    pub(super) properties: Properties,
    /// Bundles are length/magic checked but never instantiated or interpreted.
    pub(super) extras_omitted: bool,
}
// Debug can appear in assertion failures; never emit any UI-derived fields.
impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Node { contents: [redacted] }")
    }
}
impl Default for Node {
    fn default() -> Self {
        Self {
            source_id: UNDEFINED_NODE,
            window_id: -1,
            parent_id: UNDEFINED_NODE,
            connection_id: -1,
            children: Vec::new(),
            bounds_in_parent: Bounds::default(),
            bounds_in_screen: Bounds::default(),
            bounds_in_window: Bounds::default(),
            package: None,
            class: None,
            text: None,
            content_description: None,
            resource_id: None,
            unique_id: None,
            properties: Properties::default(),
            extras_omitted: false,
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(super) struct Properties {
    pub(super) checkable: bool,
    pub(super) checked: bool,
    pub(super) focusable: bool,
    pub(super) focused: bool,
    pub(super) selected: bool,
    pub(super) clickable: bool,
    pub(super) long_clickable: bool,
    pub(super) enabled: bool,
    pub(super) password: bool,
    pub(super) scrollable: bool,
    pub(super) visible_to_user: bool,
    pub(super) editable: bool,
}
impl Properties {
    fn from_bits(bits: i32) -> Self {
        let has = |mask| bits & mask != 0;
        Self {
            checkable: has(1),
            checked: has(2),
            focusable: has(4),
            focused: has(8),
            selected: has(16),
            clickable: has(32),
            long_clickable: has(64),
            enabled: has(128),
            password: has(256),
            scrollable: has(512),
            visible_to_user: has(2048),
            editable: has(4096),
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(len)
            .ok_or(Error::InvalidNode("offset_overflow"))?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or(Error::InvalidNode("truncated"))?;
        self.position = end;
        Ok(bytes)
    }
    fn int(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
    fn long(&mut self) -> Result<i64, Error> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }
    fn boolean(&mut self) -> Result<bool, Error> {
        match self.int()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::InvalidNode("boolean")),
        }
    }
    fn float(&mut self) -> Result<f32, Error> {
        let value = f32::from_bits(self.int()? as u32);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(Error::InvalidNode("non_finite_float"))
        }
    }
    fn count(&mut self, limit: usize, nullable: bool) -> Result<Option<usize>, Error> {
        let count = self.int()?;
        if count == -1 && nullable {
            return Ok(None);
        }
        let count = usize::try_from(count).map_err(|_| Error::InvalidNode("negative_length"))?;
        if count > limit {
            return Err(Error::InvalidNode("length_limit"));
        }
        Ok(Some(count))
    }
    fn string(&mut self, utf16: bool) -> Result<Option<String>, Error> {
        let Some(len) = self.count(MAX_STRING_UNITS, true)? else {
            return Ok(None);
        };
        let width = if utf16 { 2 } else { 1 };
        let size = (len + 1) * width; // bounded count, includes required null terminator
        let bytes = self.take((size + 3) & !3)?;
        if bytes[len * width..size].iter().any(|v| *v != 0) {
            return Err(Error::InvalidNode("string_terminator"));
        }
        let bytes = &bytes[..len * width];
        let value = if utf16 {
            // Bound checked before allocating; reject unpaired surrogates, preserve Unicode.
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|v| u16::from_le_bytes([v[0], v[1]]))
                .collect();
            String::from_utf16(&units).map_err(|_| Error::InvalidNode("utf16"))?
        } else {
            std::str::from_utf8(bytes)
                .map_err(|_| Error::InvalidNode("utf8"))?
                .to_owned()
        };
        Ok(Some(value))
    }
    fn text(&mut self) -> Result<Option<String>, Error> {
        let kind = self.int()?;
        if kind != 0 && kind != 1 {
            return Err(Error::InvalidNode("char_sequence_kind"));
        }
        let text = self.string(false)?;
        if kind == 0 && text.is_some() && self.int()? != 0 {
            return Err(Error::UnsupportedNodeLayout("char_sequence_span"));
        }
        Ok(text)
    }
    fn bounds(&mut self) -> Result<Bounds, Error> {
        // NodeInfo uses top,bottom,left,right (not Rect's usual parcel order).
        let top = self.int()?;
        let bottom = self.int()?;
        let left = self.int()?;
        let right = self.int()?;
        if left > right || top > bottom {
            return Err(Error::InvalidNode("inverted_bounds"));
        }
        Ok(Bounds {
            left,
            top,
            right,
            bottom,
        })
    }
    fn node(&mut self) -> Result<Node, Error> {
        let mask = self.long()? as u64;
        if mask >> 45 != 0 {
            return Err(Error::UnsupportedNodeLayout("field_mask"));
        }
        if mask & ((1 << 40) | (1 << 41) | (1 << 42) | (1 << 43)) != 0 {
            return Err(Error::UnsupportedNodeLayout("touch_rendering_or_leash"));
        }
        let mut node = Node::default();
        for field in 0..45 {
            if mask & (1 << field) == 0 {
                continue;
            }
            match field {
                0 => {
                    self.boolean()?;
                }
                1 => node.source_id = self.long()?,
                2 => {
                    node.window_id = self.int()?;
                    if node.window_id < -1 {
                        return Err(Error::InvalidNode("window_id"));
                    }
                }
                3 => node.parent_id = self.long()?,
                4..=8 | 44 => {
                    self.long()?;
                }
                9 => {
                    node.connection_id = self.int()?;
                    if node.connection_id < -1 {
                        return Err(Error::InvalidNode("connection_id"));
                    }
                }
                10 => {
                    let count = self.count(MAX_CHILDREN, false)?.expect("non-null count");
                    // Check bytes before reserving, even for a malicious declared count.
                    if count > (self.data.len() - self.position) / 8 {
                        return Err(Error::InvalidNode("truncated_children"));
                    }
                    node.children.reserve(count);
                    for _ in 0..count {
                        node.children.push(self.long()?);
                    }
                }
                11 => node.bounds_in_parent = self.bounds()?,
                12 => node.bounds_in_screen = self.bounds()?,
                13 => node.bounds_in_window = self.bounds()?,
                14 => {
                    self.long()?; // standard action bitmap
                    let count = self.count(MAX_COLLECTION, false)?.expect("non-null count");
                    for _ in 0..count {
                        self.int()?;
                        self.text()?;
                    }
                }
                15..=16 | 30..=34 => {
                    self.int()?;
                }
                17 => node.properties = Properties::from_bits(self.int()?),
                18 => node.package = self.text()?,
                19 => node.class = self.text()?,
                20 => node.text = self.text()?,
                21..=23 | 25..=27 => {
                    self.text()?;
                }
                24 => node.content_description = self.text()?,
                28 => node.resource_id = self.string(true)?,
                29 => node.unique_id = self.string(true)?,
                35 => {
                    for _ in 0..self.count(MAX_COLLECTION, true)?.unwrap_or(0) {
                        self.string(true)?;
                    }
                }
                36 => {
                    let len = self
                        .count(contract::MAX_CALLBACK_BYTES as usize, true)?
                        .unwrap_or(0);
                    if len > 0 {
                        if !len.is_multiple_of(4) {
                            return Err(Error::InvalidNode("bundle_alignment"));
                        }
                        let magic = self.int()?;
                        if magic != 0x4c444e42 && magic != 0x4c444e44 {
                            return Err(Error::InvalidNode("bundle_magic"));
                        }
                        self.take(len)?; // contents deliberately opaque; never instantiate Parcelables
                    }
                    node.extras_omitted = true;
                }
                37 => {
                    let kind = self.int()?;
                    let min = self.float()?;
                    let max = self.float()?;
                    let current = self.float()?;
                    if !(0..=2).contains(&kind) || min > max || current < min || current > max {
                        return Err(Error::InvalidNode("range"));
                    }
                }
                38 => {
                    let rows = self.int()?;
                    let columns = self.int()?;
                    self.boolean()?;
                    let selection = self.int()?;
                    if rows < 0 || columns < 0 || !(0..=2).contains(&selection) {
                        return Err(Error::InvalidNode("collection"));
                    }
                }
                39 => {
                    self.string(true)?;
                    let row = self.int()?;
                    let row_span = self.int()?;
                    self.string(true)?;
                    let col = self.int()?;
                    let col_span = self.int()?;
                    self.boolean()?;
                    self.boolean()?;
                    if row < 0 || col < 0 || row_span < 0 || col_span < 0 {
                        return Err(Error::InvalidNode("collection_item"));
                    }
                }
                _ => return Err(Error::UnsupportedNodeLayout("field")),
            }
        }
        Ok(node)
    }
}

/// Decode exactly callback arguments, excluding Binder interface-token header.
/// Runtime raw bytes are temporary, never persisted or returned to a caller.
pub(super) fn decode(data: &[u8]) -> Result<Envelope, Error> {
    if data.len() > contract::MAX_CALLBACK_BYTES as usize || !data.len().is_multiple_of(4) {
        return Err(Error::InvalidNode("parcel_size"));
    }
    let mut reader = Reader { data, position: 0 };
    let present = reader.boolean()?;
    let start = reader.position;
    let node = if present { Some(reader.node()?) } else { None };
    let size = reader.position - start;
    let id = reader.int()?;
    if id <= 0 || reader.position != data.len() {
        return Err(Error::InvalidNode("interaction_id_or_trailing_data"));
    }
    Ok(Envelope {
        interaction_id: id,
        node_present: present,
        opaque_node_bytes: size as u32,
        fields_decoded: true,
        node,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn int(v: i32) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }
    fn long(v: i64) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }
    fn string(s: Option<&str>, utf16: bool) -> Vec<u8> {
        let Some(s) = s else {
            return int(-1);
        };
        let mut data = if utf16 {
            let units: Vec<u16> = s.encode_utf16().collect();
            let mut data = int(units.len() as i32);
            for unit in units {
                data.extend(unit.to_le_bytes());
            }
            data.extend([0, 0]);
            data
        } else {
            let mut data = int(s.len() as i32);
            data.extend(s.as_bytes());
            data.push(0);
            data
        };
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
        data
    }
    fn text(s: Option<&str>) -> Vec<u8> {
        [int(1), string(s, false)].concat()
    }
    fn callback(mut fields: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
        fields.sort_by_key(|(bit, _)| *bit);
        let mask = fields
            .iter()
            .fold(0_u64, |mask, (bit, _)| mask | (1_u64 << bit));
        let mut data = [int(1), long(mask as i64)].concat();
        for (_, bytes) in fields {
            data.extend(bytes);
        }
        data.extend(int(71));
        data
    }
    fn sample() -> Vec<u8> {
        callback(vec![
            (0, int(1)),
            (1, long(0x1234)),
            (2, int(8)),
            (3, long(0x5678)),
            (9, int(4)),
            (10, [int(2), long(90), long(91)].concat()),
            (12, [int(-5), int(250), int(-10), int(400)].concat()),
            (
                14,
                [long(3), int(1), int(0x7f00001), text(Some("ação"))].concat(),
            ),
            (
                17,
                int(1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 | 256 | 512 | 2048 | 4096),
            ),
            (18, text(Some("org.geppetto.carlo.fixture"))),
            (19, text(Some("android.view.View"))),
            (20, text(Some("ação 🪵 <&>\0終"))),
            (24, text(None)),
            (28, string(Some("fixture:id/raiz_木"), true)),
            (29, string(Some("único🪵"), true)),
        ])
    }
    #[test]
    fn decodes_unicode_ids_bounds_children_actions_and_all_core_booleans() {
        let envelope = decode(&sample()).unwrap();
        assert_eq!(envelope.interaction_id, 71);
        assert!(envelope.fields_decoded);
        let node = envelope.node.unwrap();
        assert_eq!(node.source_id, 0x1234);
        assert_eq!(node.window_id, 8);
        assert_eq!(node.children, [90, 91]);
        assert_eq!(node.parent_id, 0x5678);
        assert_eq!(node.text.as_deref(), Some("ação 🪵 <&>\0終"));
        assert_eq!(node.resource_id.as_deref(), Some("fixture:id/raiz_木"));
        assert_eq!(node.unique_id.as_deref(), Some("único🪵"));
        assert_eq!(node.content_description, None);
        assert_eq!(
            node.bounds_in_screen,
            Bounds {
                left: -10,
                top: -5,
                right: 400,
                bottom: 250
            }
        );
        assert_eq!(
            node.properties,
            Properties {
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
                editable: true
            }
        );
        assert!(!format!("{node:?}").contains("ação"));
    }
    #[test]
    fn null_defaults_empty_spans_and_optional_fields_preserve_alignment() {
        let absent = decode(&[int(0), int(2)].concat()).unwrap();
        assert!(absent.node.is_none());
        let empty = decode(&callback(vec![])).unwrap().node.unwrap();
        assert_eq!(empty, Node::default());
        let body = callback(vec![
            (20, [int(0), string(Some("plain"), false), int(0)].concat()),
            (
                35,
                [int(2), string(None, true), string(Some("key"), true)].concat(),
            ),
            (36, [int(4), int(0x4c444e42), int(0)].concat()),
            (
                37,
                [
                    int(1),
                    int(0_f32.to_bits() as i32),
                    int(10_f32.to_bits() as i32),
                    int(3_f32.to_bits() as i32),
                ]
                .concat(),
            ),
            (38, [int(1), int(2), int(0), int(1)].concat()),
            (
                39,
                [
                    string(None, true),
                    int(0),
                    int(1),
                    string(Some("col"), true),
                    int(1),
                    int(1),
                    int(0),
                    int(1),
                ]
                .concat(),
            ),
            (44, long(42)),
        ]);
        let result = decode(&body).unwrap();
        assert_eq!(result.interaction_id, 71);
        let node = result.node.unwrap();
        assert_eq!(node.text.as_deref(), Some("plain"));
        assert!(node.extras_omitted);
    }
    #[test]
    fn every_truncation_and_trailing_bytes_fail_without_partial_success() {
        let data = sample();
        for len in 0..data.len() {
            assert!(
                decode(&data[..len]).is_err(),
                "accepted truncated length {len}"
            );
        }
        let mut extra = data;
        extra.extend(int(0));
        assert!(decode(&extra).is_err());
        assert!(decode(&vec![0; contract::MAX_CALLBACK_BYTES as usize + 4]).is_err());
    }
    #[test]
    fn rejects_unknown_layouts_spans_invalid_utf_and_allocation_attacks() {
        for (field, bytes) in [
            (45, vec![]),
            (40, vec![]),
            (41, vec![]),
            (42, vec![]),
            (43, vec![]),
            (0, int(2)),
            (10, int(-1)),
            (10, int(i32::MAX)),
            (10, int(1000)),
            (14, [long(0), int(129)].concat()),
            (20, [int(0), string(Some("styled"), false), int(7)].concat()),
            (20, [int(9), string(Some("unknown"), false)].concat()),
            (20, [int(1), int(i32::MAX)].concat()),
            (20, [int(1), int(-2)].concat()),
            (20, [int(1), int(1), vec![0xff, 0, 0, 0]].concat()),
            (20, [int(1), int(1), vec![b'x', b'y', 0, 0]].concat()),
            (28, [int(1), vec![0, 0xd8, 0, 0]].concat()),
            (12, [int(20), int(10), int(0), int(5)].concat()),
            (35, int(129)),
            (36, [int(8), int(0)].concat()),
            (36, [int(3), int(0x4c444e42)].concat()),
            (36, [int(65532), int(0x4c444e42)].concat()),
            (
                37,
                [int(0), int(f32::NAN.to_bits() as i32), int(0), int(0)].concat(),
            ),
            (38, [int(-1), int(2), int(0), int(0)].concat()),
        ] {
            assert!(
                decode(&callback(vec![(field, bytes)])).is_err(),
                "field {field}"
            );
        }
        for id in [-1, 0] {
            assert!(decode(&[int(0), int(id)].concat()).is_err());
        }
    }
    #[test]
    fn deterministic_mutations_never_panic_or_allocate_from_unbounded_lengths() {
        let data = sample();
        for offset in 0..data.len() {
            for value in [0, 0x7f, 0xff] {
                let mut mutated = data.clone();
                mutated[offset] = value;
                let _ = decode(&mutated);
            }
        }
    }
}
