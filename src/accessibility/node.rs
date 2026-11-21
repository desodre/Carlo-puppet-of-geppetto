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
    /// Bound on one string, in code units, checked before any allocation.
    string_units: usize,
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
        let Some(len) = self.count(self.string_units, true)? else {
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

// ---------------------------------------------------------------------------
// API 36 (Samsung A16 5G pinned build) layout. A second explicit table: nothing
// above depends on it. Read from that build's framework.jar; never executed.

/// One string may fill a callback; the parcel bound already limits the bytes.
const MAX_STRING_UNITS_36: usize = 100_000;
/// A node cannot list more children than a snapshot may hold nodes.
const MAX_CHILDREN_36: usize = 10_000;
/// `AccessibilityNodeInfo` non-default field bits of the pinned API 36 build, in
/// write order. Names appear in typed errors; they never carry UI contents.
const FIELDS_36: [&str; 50] = [
    "sealed",
    "source_node_id",
    "window_id",
    "parent_node_id",
    "label_for_id",
    "labeled_by_id",
    "labeled_by_ids",
    "traversal_before",
    "traversal_after",
    "min_duration_between_content_changes",
    "connection_id",
    "child_node_ids",
    "bounds_in_parent",
    "bounds_in_screen",
    "bounds_in_window",
    "actions",
    "max_text_length",
    "movement_granularities",
    "boolean_properties",
    "package_name",
    "class_name",
    "text",
    "hint_text",
    "error",
    "state_description",
    "content_description",
    "supplemental_description",
    "pane_title",
    "tooltip_text",
    "container_title",
    "view_id_resource_name",
    "unique_id",
    "text_selection_start",
    "text_selection_end",
    "input_type",
    "live_region",
    "drawing_order",
    "extra_data_keys",
    "extras",
    "range_info",
    "collection_info",
    "collection_item_info",
    "touch_delegate_info",
    "extra_rendering_info",
    "leashed_child",
    "leashed_parent",
    "leashed_parent_node_id",
    "selection",
    "checked",
    "expanded_state",
];

/// Attach the API 36 field name to a primitive failure.
fn at(field: &'static str, error: Error) -> Error {
    match error {
        Error::InvalidNode(reason) => Error::InvalidNodeField { field, reason },
        Error::UnsupportedNodeLayout(reason) => Error::UnsupportedNodeField { field, reason },
        other => other,
    }
}

/// Nodes of one API 36 result callback, in the order the app wrote them.
pub(super) struct Batch {
    pub(super) interaction_id: i32,
    pub(super) nodes: Vec<Node>,
}
// Never print node contents, not even through a failed assertion.
impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Batch {{ nodes: {}, contents: [redacted] }}",
            self.nodes.len()
        )
    }
}

impl Reader<'_> {
    /// Four ints in NodeInfo order. Unlike the API 34 proof, an app-provided
    /// rectangle is passed through unchanged: a dump must not fail on odd geometry.
    fn bounds36(&mut self) -> Result<Bounds, Error> {
        let top = self.int()?;
        let bottom = self.int()?;
        let left = self.int()?;
        let right = self.int()?;
        Ok(Bounds {
            left,
            top,
            right,
            bottom,
        })
    }
    fn longs36(&mut self, limit: usize) -> Result<Vec<i64>, Error> {
        let count = self.count(limit, false)?.expect("non-null count");
        // Check bytes before reserving, even for a malicious declared count.
        if count > (self.data.len() - self.position) / 8 {
            return Err(Error::InvalidNode("truncated_list"));
        }
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(self.long()?);
        }
        Ok(values)
    }
    fn node36(&mut self) -> Result<Node, Error> {
        let mask = self.long().map_err(|error| at("field_mask", error))? as u64;
        if mask >> FIELDS_36.len() != 0 {
            return Err(Error::UnsupportedNodeField {
                field: "field_mask",
                reason: "unknown_bit",
            });
        }
        // Region lists, rendering info and leash Binders have no pinned layout.
        for (offset, name) in FIELDS_36[42..=45].iter().enumerate() {
            if mask & (1 << (42 + offset)) != 0 {
                return Err(Error::UnsupportedNodeField {
                    field: name,
                    reason: "layout_not_pinned",
                });
            }
        }
        let mut node = Node::default();
        let mut checked_state = 0;
        for (bit, name) in FIELDS_36.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                self.field36(bit, &mut node, &mut checked_state)
                    .map_err(|error| at(name, error))?;
            }
        }
        // Tri-state `checked` (0 false, 1 true, 2 partial): only "true" is checked.
        node.properties.checked |= checked_state == 1;
        Ok(node)
    }
    fn field36(&mut self, bit: usize, node: &mut Node, checked: &mut i32) -> Result<(), Error> {
        match bit {
            0 => {
                self.boolean()?;
            }
            1 => node.source_id = self.long()?,
            2 => {
                node.window_id = self.int()?;
                if node.window_id < -1 {
                    return Err(Error::InvalidNode("range"));
                }
            }
            3 => node.parent_id = self.long()?,
            4 | 5 | 7..=9 | 46 => {
                self.long()?;
            }
            6 => {
                self.longs36(MAX_CHILDREN_36)?;
            }
            10 => {
                node.connection_id = self.int()?;
                if node.connection_id < -1 {
                    return Err(Error::InvalidNode("range"));
                }
            }
            11 => node.children = self.longs36(MAX_CHILDREN_36)?,
            12 => node.bounds_in_parent = self.bounds36()?,
            13 => node.bounds_in_screen = self.bounds36()?,
            14 => node.bounds_in_window = self.bounds36()?,
            15 => {
                self.long()?; // standard action bitmap
                let count = self.count(MAX_COLLECTION, false)?.expect("non-null count");
                for _ in 0..count {
                    self.int()?;
                    self.text()?;
                }
            }
            16 | 17 | 32..=36 => {
                self.int()?;
            }
            18 => node.properties = Properties::from_bits(self.int()?),
            19 => node.package = self.text()?,
            20 => node.class = self.text()?,
            21 => node.text = self.text()?,
            25 => node.content_description = self.text()?,
            22..=24 | 26..=29 => {
                self.text()?;
            }
            30 => node.resource_id = self.string(true)?,
            31 => node.unique_id = self.string(true)?,
            37 => {
                for _ in 0..self.count(MAX_COLLECTION, true)?.unwrap_or(0) {
                    self.string(true)?;
                }
            }
            38 => {
                let len = self
                    .count(contract::MAX_BATCH_BYTES as usize, true)?
                    .unwrap_or(0);
                if len > 0 {
                    if !len.is_multiple_of(4) {
                        return Err(Error::InvalidNode("bundle_alignment"));
                    }
                    let magic = self.int()?;
                    if magic != 0x4c444e42 && magic != 0x4c444e44 {
                        return Err(Error::InvalidNode("bundle_magic"));
                    }
                    self.take(len)?; // opaque; Parcelables are never instantiated
                    // Android 16 `BaseBundle.writeToParcelInner` appends
                    // `writeBoolean(mHasIntent)` after a non-empty payload (read from
                    // the pinned framework.jar; an empty Bundle is the single 0).
                    self.boolean()?;
                }
                node.extras_omitted = true;
            }
            39 => {
                // Type 3 is RANGE_TYPE_INDETERMINATE. The three floats are app
                // data that a dump does not use: consumed without interpretation.
                if !(0..=3).contains(&self.int()?) {
                    return Err(Error::InvalidNode("range_type"));
                }
                self.take(12)?;
            }
            40 => {
                // rows, columns (-1 = unknown is legitimate), hierarchical,
                // selectionMode, itemCount, importantForAccessibilityItemCount.
                self.int()?;
                self.int()?;
                self.boolean()?;
                self.int()?;
                self.int()?;
                self.int()?;
            }
            41 => {
                self.string(true)?; // rowTitle
                self.int()?;
                self.int()?;
                self.string(true)?; // columnTitle
                self.int()?;
                self.int()?;
                self.boolean()?; // heading
                self.boolean()?; // selected
                self.int()?; // sortDirection, always written
            }
            47 => {
                // Two selection ends: i32 offset + i64 node id each.
                self.take(24)?;
            }
            48 => {
                *checked = self.int()?;
                if !(0..=2).contains(checked) {
                    return Err(Error::InvalidNode("range"));
                }
            }
            49 => {
                if !(0..=3).contains(&self.int()?) {
                    return Err(Error::InvalidNode("range"));
                }
            }
            _ => return Err(Error::UnsupportedNodeLayout("layout_not_pinned")),
        }
        Ok(())
    }
}

/// Decode the arguments of one API 36 result callback (`code` 1 or 2), excluding
/// the interface token. A null node answered with the sentinel interaction id is
/// [`Error::WindowGone`]. Null list items are skipped. Nothing is resynchronised:
/// the first field that does not match fails the whole callback, by name.
pub(super) fn decode36(code: u32, data: &[u8]) -> Result<Batch, Error> {
    let envelope = |reason| Error::InvalidNodeField {
        field: "callback",
        reason,
    };
    if data.len() > contract::MAX_BATCH_BYTES as usize || !data.len().is_multiple_of(4) {
        return Err(envelope("parcel_size"));
    }
    let mut reader = Reader {
        data,
        position: 0,
        string_units: MAX_STRING_UNITS_36,
    };
    let mut nodes = Vec::new();
    match code {
        contract::NODE_RESULT => {
            if reader.boolean().map_err(|error| at("presence", error))? {
                nodes.push(reader.node36()?);
            }
        }
        contract::NODE_RESULTS => {
            let count = reader
                .count(contract::MAX_BATCH_NODES, true)
                .map_err(|error| at("list_count", error))?
                .unwrap_or(0);
            for _ in 0..count {
                if reader
                    .boolean()
                    .map_err(|error| at("item_presence", error))?
                {
                    nodes.push(reader.node36()?);
                }
            }
        }
        _ => return Err(envelope("transaction_code")),
    }
    let id = reader.int().map_err(|error| at("interaction_id", error))?;
    if reader.position != data.len() {
        return Err(envelope("trailing_data"));
    }
    if code == contract::NODE_RESULT && nodes.is_empty() && id == contract::WINDOW_GONE_INTERACTION
    {
        return Err(Error::WindowGone);
    }
    if id <= 0 {
        return Err(Error::InvalidNodeField {
            field: "interaction_id",
            reason: "range",
        });
    }
    Ok(Batch {
        interaction_id: id,
        nodes,
    })
}

/// Decode exactly callback arguments, excluding Binder interface-token header.
/// Runtime raw bytes are temporary, never persisted or returned to a caller.
pub(super) fn decode(data: &[u8]) -> Result<Envelope, Error> {
    if data.len() > contract::MAX_CALLBACK_BYTES as usize || !data.len().is_multiple_of(4) {
        return Err(Error::InvalidNode("parcel_size"));
    }
    let mut reader = Reader {
        data,
        position: 0,
        string_units: MAX_STRING_UNITS,
    };
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

#[cfg(test)]
mod tests36 {
    use super::*;
    fn int(v: i32) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }
    fn long(v: i64) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }
    fn float(v: f32) -> Vec<u8> {
        int(v.to_bits() as i32)
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
    /// The node body: mask, then the set fields in bit order.
    fn node(mut fields: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
        fields.sort_by_key(|(bit, _)| *bit);
        let mask = fields
            .iter()
            .fold(0_u64, |mask, (bit, _)| mask | (1_u64 << bit));
        let mut data = long(mask as i64);
        for (_, bytes) in fields {
            data.extend(bytes);
        }
        data
    }
    fn single(fields: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
        [int(1), node(fields), int(71)].concat()
    }
    fn list(items: Vec<Option<Vec<u8>>>, id: i32) -> Vec<u8> {
        let mut data = int(items.len() as i32);
        for item in items {
            match item {
                Some(body) => {
                    data.extend(int(1));
                    data.extend(body);
                }
                None => data.extend(int(0)),
            }
        }
        data.extend(int(id));
        data
    }
    /// Every pinned field that has a layout, with a distinct value in each slot.
    fn full() -> Vec<(u32, Vec<u8>)> {
        vec![
            (0, int(1)),
            (1, long(0x1234)),
            (2, int(8)),
            (3, long(0x5678)),
            (4, long(41)),
            (5, long(42)),
            (6, [int(2), long(43), long(44)].concat()),
            (7, long(45)),
            (8, long(46)),
            (9, long(47)),
            (10, int(4)),
            (11, [int(3), long(90), long(91), long(92)].concat()),
            (12, [int(1), int(2), int(3), int(4)].concat()),
            (13, [int(-5), int(250), int(-10), int(400)].concat()),
            (14, [int(5), int(6), int(7), int(8)].concat()),
            (
                15,
                [long(3), int(1), int(0x7f00001), text(Some("ação"))].concat(),
            ),
            (16, int(-1)),
            (17, int(31)),
            (18, int(1 | 4 | 8 | 16 | 32 | 64 | 128 | 256 | 512 | 2048)),
            (19, text(Some("org.geppetto.carlo.fixture"))),
            (20, text(Some("android.widget.CheckBox"))),
            (21, text(Some("ação 🪵 <&>\0終"))),
            (22, text(Some("hint"))),
            (23, text(None)),
            (24, text(Some("state"))),
            (25, text(Some("descrição"))),
            (26, text(Some("supplemental"))),
            (27, text(Some("pane"))),
            (28, text(Some("tooltip"))),
            (29, text(Some("container"))),
            (30, string(Some("fixture:id/raiz_木"), true)),
            (31, string(Some("único🪵"), true)),
            (32, int(1)),
            (33, int(2)),
            (34, int(3)),
            (35, int(1)),
            (36, int(9)),
            (
                37,
                [int(2), string(None, true), string(Some("key"), true)].concat(),
            ),
            // length, magic, 4 payload bytes, then the Android 16 mHasIntent boolean
            (38, [int(4), int(0x4c444e42), int(0), int(0)].concat()),
            (39, [int(3), float(0.0), float(0.0), float(0.0)].concat()),
            (
                40,
                [int(-1), int(1), int(0), int(1), int(12), int(11)].concat(),
            ),
            (
                41,
                [
                    string(None, true),
                    int(0),
                    int(1),
                    string(Some("col"), true),
                    int(1),
                    int(1),
                    int(0),
                    int(1),
                    int(2),
                ]
                .concat(),
            ),
            (46, long(77)),
            (47, [int(1), long(5), int(3), long(6)].concat()),
            (48, int(1)),
            (49, int(3)),
        ]
    }
    fn field(result: Result<Batch, Error>) -> (&'static str, &'static str) {
        match result {
            Err(Error::InvalidNodeField { field, reason })
            | Err(Error::UnsupportedNodeField { field, reason }) => (field, reason),
            other => panic!("expected a field error, got {other:?}"),
        }
    }

    #[test]
    fn decodes_every_pinned_field_of_the_api36_layout() {
        let batch = decode36(1, &single(full())).unwrap();
        assert_eq!(batch.interaction_id, 71);
        let node = &batch.nodes[0];
        assert_eq!(node.source_id, 0x1234);
        assert_eq!(node.window_id, 8);
        assert_eq!(node.parent_id, 0x5678);
        assert_eq!(node.connection_id, 4);
        assert_eq!(node.children, [90, 91, 92]);
        assert_eq!(
            node.bounds_in_screen,
            Bounds {
                left: -10,
                top: -5,
                right: 400,
                bottom: 250
            }
        );
        assert_eq!(node.bounds_in_parent.left, 3);
        assert_eq!(node.bounds_in_window.right, 8);
        assert_eq!(node.package.as_deref(), Some("org.geppetto.carlo.fixture"));
        assert_eq!(node.class.as_deref(), Some("android.widget.CheckBox"));
        assert_eq!(node.text.as_deref(), Some("ação 🪵 <&>\0終"));
        assert_eq!(node.content_description.as_deref(), Some("descrição"));
        assert_eq!(node.resource_id.as_deref(), Some("fixture:id/raiz_木"));
        assert_eq!(node.unique_id.as_deref(), Some("único🪵"));
        assert!(node.extras_omitted);
        // Boolean bit 2 was clear: the tri-state field alone made it checked.
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
                editable: false
            }
        );
        assert!(!format!("{batch:?}").contains("ação"));
        assert_eq!(
            decode36(1, &single(vec![])).unwrap().nodes,
            [Node::default()]
        );
    }

    #[test]
    fn shifted_bits_are_not_read_with_the_api34_positions() {
        // API 34 bit 6 was traversalBefore (i64); here it is labeledByIds (a list).
        let ids = decode36(1, &single(vec![(6, [int(1), long(7)].concat())])).unwrap();
        assert_eq!(ids.nodes[0], Node::default());
        assert_eq!(
            field(decode36(1, &single(vec![(6, long(7))]))),
            ("labeled_by_ids", "truncated_list")
        );
        // API 34 bit 24 was contentDescription; here it is stateDescription.
        let state = decode36(1, &single(vec![(24, text(Some("x")))])).unwrap();
        assert_eq!(state.nodes[0].content_description, None);
        let description = decode36(1, &single(vec![(25, text(Some("x")))])).unwrap();
        assert_eq!(
            description.nodes[0].content_description.as_deref(),
            Some("x")
        );
        // API 34 bits 28/29 were the two String16 ids; here they are CharSequences.
        assert!(decode36(1, &single(vec![(28, text(Some("tip")))])).is_ok());
        let id = decode36(1, &single(vec![(30, string(Some("a:id/b"), true))])).unwrap();
        assert_eq!(id.nodes[0].resource_id.as_deref(), Some("a:id/b"));
        // The same body means different things to the two decoders.
        let old = decode(&single(vec![(17, int(2)), (18, text(Some("pkg")))])).unwrap();
        assert!(old.node.unwrap().properties.checked);
        assert!(decode36(1, &single(vec![(17, int(2)), (18, text(Some("pkg")))])).is_err());
    }

    #[test]
    fn tri_state_checked_range_types_and_new_collection_ints() {
        for (state, bits, checked) in [
            (0, 0, false),
            (1, 0, true),
            (2, 0, false),
            (0, 2, true),
            (2, 2, true),
        ] {
            let batch = decode36(1, &single(vec![(18, int(bits)), (48, int(state))])).unwrap();
            assert_eq!(batch.nodes[0].properties.checked, checked, "{state} {bits}");
        }
        assert_eq!(
            field(decode36(1, &single(vec![(48, int(3))]))),
            ("checked", "range")
        );
        assert_eq!(
            field(decode36(1, &single(vec![(49, int(4))]))),
            ("expanded_state", "range")
        );
        let range = |kind: i32| {
            single(vec![(
                39,
                [int(kind), float(5.0), float(1.0), float(f32::NAN)].concat(),
            )])
        };
        for kind in 0..=3 {
            assert!(decode36(1, &range(kind)).is_ok(), "range type {kind}");
        }
        assert_eq!(field(decode36(1, &range(4))), ("range_info", "range_type"));
        assert_eq!(field(decode36(1, &range(-1))), ("range_info", "range_type"));
        // Six ints: the API 34 four-int body is short by two.
        let old = single(vec![(40, [int(1), int(2), int(0), int(1)].concat())]);
        assert!(decode36(1, &old).is_err());
        let hierarchical = |flag: i32| {
            single(vec![(
                40,
                [int(1), int(2), int(flag), int(1), int(-1), int(-1)].concat(),
            )])
        };
        assert!(decode36(1, &hierarchical(1)).is_ok());
        assert_eq!(
            field(decode36(1, &hierarchical(2))),
            ("collection_info", "boolean")
        );
        // Item info without the trailing sortDirection is truncated.
        let mut item = full().into_iter().find(|(bit, _)| *bit == 41).unwrap().1;
        item.truncate(item.len() - 4);
        assert!(decode36(1, &single(vec![(41, item)])).is_err());
    }

    #[test]
    fn unknown_bits_unpinned_fields_and_spans_are_typed_by_field() {
        for bit in 50..64 {
            assert_eq!(
                field(decode36(1, &single(vec![(bit, vec![])]))),
                ("field_mask", "unknown_bit"),
                "bit {bit}"
            );
        }
        for (bit, name) in [
            (42, "touch_delegate_info"),
            (43, "extra_rendering_info"),
            (44, "leashed_child"),
            (45, "leashed_parent"),
        ] {
            assert_eq!(
                decode36(1, &single(vec![(bit, vec![])])).unwrap_err(),
                Error::UnsupportedNodeField {
                    field: name,
                    reason: "layout_not_pinned"
                }
            );
        }
        let spanned = [int(0), string(Some("styled"), false), int(7)].concat();
        for (bit, name) in [
            (21, "text"),
            (25, "content_description"),
            (26, "supplemental_description"),
            (29, "container_title"),
        ] {
            assert_eq!(
                decode36(1, &single(vec![(bit, spanned.clone())])).unwrap_err(),
                Error::UnsupportedNodeField {
                    field: name,
                    reason: "char_sequence_span"
                }
            );
        }
        let unspanned = [int(0), string(Some("plain"), false), int(0)].concat();
        let batch = decode36(1, &single(vec![(21, unspanned)])).unwrap();
        assert_eq!(batch.nodes[0].text.as_deref(), Some("plain"));
        for (bit, bytes, expected) in [
            (0, int(2), ("sealed", "boolean")),
            (2, int(-2), ("window_id", "range")),
            (10, int(-2), ("connection_id", "range")),
            (11, int(-1), ("child_node_ids", "negative_length")),
            (11, int(10_001), ("child_node_ids", "length_limit")),
            (11, int(9_000), ("child_node_ids", "truncated_list")),
            (
                15,
                [long(0), int(129)].concat(),
                ("actions", "length_limit"),
            ),
            (
                21,
                [int(9), string(Some("x"), false)].concat(),
                ("text", "char_sequence_kind"),
            ),
            (
                21,
                [int(1), int(100_001)].concat(),
                ("text", "length_limit"),
            ),
            (
                21,
                [int(1), int(1), vec![0xff, 0, 0, 0]].concat(),
                ("text", "utf8"),
            ),
            (
                30,
                [int(1), vec![0, 0xd8, 0, 0]].concat(),
                ("view_id_resource_name", "utf16"),
            ),
            (37, int(129), ("extra_data_keys", "length_limit")),
            (38, [int(8), int(0)].concat(), ("extras", "bundle_magic")),
            (
                38,
                [int(3), int(0x4c444e42)].concat(),
                ("extras", "bundle_alignment"),
            ),
        ] {
            assert_eq!(
                field(decode36(1, &single(vec![(bit, bytes)]))),
                expected,
                "bit {bit}"
            );
        }
        // Odd geometry is app data, not a malformed parcel.
        let inverted = [int(20), int(10), int(9), int(5)].concat();
        let batch = decode36(1, &single(vec![(13, inverted)])).unwrap();
        assert_eq!(batch.nodes[0].bounds_in_screen.top, 20);
        // A string longer than the API 34 bound is accepted here.
        let long_text = "x".repeat(MAX_STRING_UNITS + 1);
        let batch = decode36(1, &single(vec![(21, text(Some(&long_text)))])).unwrap();
        assert_eq!(
            batch.nodes[0].text.as_ref().unwrap().len(),
            MAX_STRING_UNITS + 1
        );
    }

    #[test]
    fn typed_lists_decode_counts_null_lists_and_null_items() {
        let a = node(vec![(1, long(1)), (2, int(8))]);
        let b = node(vec![(1, long(2)), (2, int(8)), (21, text(Some("b")))]);
        let batch = decode36(2, &list(vec![Some(a.clone()), None, Some(b.clone())], 12)).unwrap();
        assert_eq!(batch.interaction_id, 12);
        assert_eq!(batch.nodes.len(), 2);
        assert_eq!((batch.nodes[0].source_id, batch.nodes[1].source_id), (1, 2));
        assert_eq!(batch.nodes[1].text.as_deref(), Some("b"));
        assert!(decode36(2, &list(vec![], 12)).unwrap().nodes.is_empty());
        assert!(
            decode36(2, &list(vec![None, None], 12))
                .unwrap()
                .nodes
                .is_empty()
        );
        // A null list is -1 with no items.
        let null = decode36(2, &[int(-1), int(12)].concat()).unwrap();
        assert!(null.nodes.is_empty() && null.interaction_id == 12);
        let full_list = decode36(2, &list(vec![Some(a.clone()); 64], 12)).unwrap();
        assert_eq!(full_list.nodes.len(), 64);
        assert_eq!(
            field(decode36(2, &list(vec![Some(a.clone()); 65], 12))),
            ("list_count", "length_limit")
        );
        assert_eq!(
            field(decode36(2, &[int(-2), int(12)].concat())),
            ("list_count", "negative_length")
        );
        assert_eq!(
            field(decode36(2, &[int(1), int(2), int(12)].concat())),
            ("item_presence", "boolean")
        );
        // A failing node fails the whole callback: nothing after it is trusted.
        let spanned = node(vec![(
            21,
            [int(0), string(Some("s"), false), int(3)].concat(),
        )]);
        assert_eq!(
            field(decode36(
                2,
                &list(vec![Some(a.clone()), Some(spanned), Some(b)], 12)
            )),
            ("text", "char_sequence_span")
        );
        for id in [0, -1] {
            assert_eq!(
                field(decode36(2, &list(vec![Some(a.clone())], id))),
                ("interaction_id", "range")
            );
        }
        let mut trailing = list(vec![Some(a.clone())], 12);
        trailing.extend(int(0));
        assert_eq!(field(decode36(2, &trailing)), ("callback", "trailing_data"));
        assert_eq!(
            field(decode36(3, &list(vec![Some(a.clone())], 12))),
            ("callback", "transaction_code")
        );
        assert_eq!(
            field(decode36(
                2,
                &vec![0; contract::MAX_BATCH_BYTES as usize + 4]
            )),
            ("callback", "parcel_size")
        );
        // Larger than the API 34 bound, inside the API 36 one.
        let big = "é".repeat(40_000);
        let wide = node(vec![(1, long(3)), (2, int(8)), (21, text(Some(&big)))]);
        let data = list(vec![Some(wide), Some(a)], 12);
        assert!(data.len() > contract::MAX_CALLBACK_BYTES as usize);
        assert_eq!(decode36(2, &data).unwrap().nodes.len(), 2);
        assert!(decode(&data).is_err());
    }

    #[test]
    fn the_invalid_window_sentinel_is_window_gone_not_a_stale_answer() {
        let sentinel = [int(0), int(i32::MAX)].concat();
        assert_eq!(decode36(1, &sentinel).unwrap_err(), Error::WindowGone);
        // A null node with the request's own id is an ordinary "no such node".
        let null = decode36(1, &[int(0), int(5)].concat()).unwrap();
        assert!(null.nodes.is_empty() && null.interaction_id == 5);
        // The sentinel only exists on the single-result transaction.
        let listed = decode36(2, &list(vec![], i32::MAX)).unwrap();
        assert_eq!(listed.interaction_id, i32::MAX);
        let present = decode36(1, &[int(1), node(vec![]), int(i32::MAX)].concat()).unwrap();
        assert_eq!(present.nodes.len(), 1);
    }

    #[test]
    fn every_truncation_fails_and_mutations_never_panic() {
        let single_node = single(full());
        for len in 0..single_node.len() {
            assert!(
                decode36(1, &single_node[..len]).is_err(),
                "accepted truncated length {len}"
            );
        }
        let batch = list(vec![Some(node(full())), None, Some(node(full()))], 9);
        for len in 0..batch.len() {
            assert!(
                decode36(2, &batch[..len]).is_err(),
                "accepted truncated list length {len}"
            );
        }
        for data in [single_node, batch] {
            for offset in 0..data.len() {
                for value in [0, 0x7f, 0xff] {
                    let mut mutated = data.clone();
                    mutated[offset] = value;
                    let _ = decode36(1, &mutated);
                    let _ = decode36(2, &mutated);
                }
            }
        }
    }
}
