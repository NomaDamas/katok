//! Minimal Apple binary-property-list (`bplist00`) reader: extracts a root-level
//! array of integers, and nothing else.
//!
//! KakaoTalk stores `NTChatRoom.displayMemberIds` as a binary plist array of the
//! room's display-member userIds (self excluded, capped to the few names the app
//! previews). We hand-roll the tiny subset needed to read that one shape rather
//! than pull a full plist dependency — mirroring `auth.rs`, which hand-scans
//! plist XML instead of taking a crate.
//!
//! Every structural anomaly degrades to `None`; this parser never panics and
//! never allocates unboundedly on a corrupt length.

/// Parse a `bplist00` blob whose root object is an array of integers, returning
/// the values in order. Returns `None` when the blob is not a binary plist or
/// its root is not an integer array.
pub(crate) fn int_array(blob: &[u8]) -> Option<Vec<i64>> {
    const HEADER: &[u8] = b"bplist00";
    const TRAILER_LEN: usize = 32;
    // A member list this long is corrupt, not real (KakaoTalk previews only a
    // handful). Refusing it stops a local-DB fault from amplifying into a huge
    // Vec / giant reconstructed chat name.
    const MAX_ELEMENTS: usize = 256;
    if blob.len() < HEADER.len() + TRAILER_LEN || &blob[..HEADER.len()] != HEADER {
        return None;
    }

    let trailer = &blob[blob.len() - TRAILER_LEN..];
    let offset_int_size = trailer[6] as usize;
    let object_ref_size = trailer[7] as usize;
    let num_objects = be_uint(&trailer[8..16])? as usize;
    let top_object = be_uint(&trailer[16..24])? as usize;
    let offset_table_offset = be_uint(&trailer[24..32])? as usize;
    if !(1..=8).contains(&offset_int_size)
        || !(1..=8).contains(&object_ref_size)
        || top_object >= num_objects
    {
        return None;
    }

    // Layout: header | object area | offset table | trailer. The object area is
    // [HEADER, offset_table_offset); the offset table must start after the header
    // and end before the trailer. Anything else is malformed, not data.
    let object_area_start = HEADER.len();
    let content_end = blob.len() - TRAILER_LEN;
    let table_len = num_objects.checked_mul(offset_int_size)?;
    let table_end = offset_table_offset.checked_add(table_len)?;
    if offset_table_offset < object_area_start || table_end > content_end {
        return None;
    }

    // Resolve an object index to a byte offset, requiring it to land inside the
    // object area (never the header, offset table, or trailer).
    let object_offset = |idx: usize| -> Option<usize> {
        let start = offset_table_offset + idx * offset_int_size;
        let off = be_uint(blob.get(start..start + offset_int_size)?)? as usize;
        (object_area_start..offset_table_offset)
            .contains(&off)
            .then_some(off)
    };

    // Root must be an array (marker high nibble 0xA).
    let root_offset = object_offset(top_object)?;
    if blob.get(root_offset)? & 0xf0 != 0xa0 {
        return None;
    }
    let (count, mut cursor) = collection_count(blob, root_offset)?;
    if count > MAX_ELEMENTS {
        return None;
    }

    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        // Element refs are part of the array object and must stay in the object
        // area, before the offset table.
        if cursor.checked_add(object_ref_size)? > offset_table_offset {
            return None;
        }
        let obj_idx = be_uint(blob.get(cursor..cursor + object_ref_size)?)? as usize;
        cursor += object_ref_size;
        if obj_idx >= num_objects {
            return None;
        }
        out.push(read_int(blob, object_offset(obj_idx)?)?);
    }
    Some(out)
}

/// Element count of an array/set object at `offset`, plus the offset where its
/// element references begin. Handles the inline nibble count and the `0xF`
/// overflow form (a following int object carries the real count).
fn collection_count(blob: &[u8], offset: usize) -> Option<(usize, usize)> {
    let nibble = (blob.get(offset)? & 0x0f) as usize;
    if nibble != 0x0f {
        return Some((nibble, offset + 1));
    }
    let size_marker = *blob.get(offset + 1)?;
    if size_marker & 0xf0 != 0x10 {
        return None;
    }
    let len = 1usize << (size_marker & 0x0f) as usize;
    let value = be_uint(blob.get(offset + 2..offset + 2 + len)?)? as usize;
    Some((value, offset + 2 + len))
}

/// Read an integer object (marker high nibble 0x1) at `offset`. Rejects widths
/// above 8 bytes, which no member userId uses.
fn read_int(blob: &[u8], offset: usize) -> Option<i64> {
    let marker = *blob.get(offset)?;
    if marker & 0xf0 != 0x10 {
        return None;
    }
    let power = (marker & 0x0f) as usize;
    if power > 3 {
        return None;
    }
    let len = 1usize << power;
    // Reject an 8-byte value above i64::MAX rather than wrapping it negative.
    i64::try_from(be_uint(blob.get(offset + 1..offset + 1 + len)?)?).ok()
}

/// Big-endian unsigned read of 1..=8 bytes.
fn be_uint(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    Some(bytes.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic fixtures produced by CPython `plistlib.dumps(obj, FMT_BINARY)`.
    // Values are arbitrary, not real userIds.

    // [1, 2, 3]
    const BPLIST_THREE: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xa3, 0x01, 0x02, 0x03, 0x10, 0x01, 0x10,
        0x02, 0x10, 0x03, 0x08, 0x0c, 0x0e, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12,
    ];
    // [123456789]
    const BPLIST_SINGLE: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xa1, 0x01, 0x12, 0x07, 0x5b, 0xcd, 0x15,
        0x08, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x0f,
    ];
    // []
    const BPLIST_EMPTY: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xa0, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09,
    ];
    // 1000..=1019 (20 elements → array count nibble 0xF overflow path)
    const BPLIST_BIG: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xaf, 0x10, 0x14, 0x01, 0x02, 0x03, 0x04,
        0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13,
        0x14, 0x11, 0x03, 0xe8, 0x11, 0x03, 0xe9, 0x11, 0x03, 0xea, 0x11, 0x03, 0xeb, 0x11, 0x03,
        0xec, 0x11, 0x03, 0xed, 0x11, 0x03, 0xee, 0x11, 0x03, 0xef, 0x11, 0x03, 0xf0, 0x11, 0x03,
        0xf1, 0x11, 0x03, 0xf2, 0x11, 0x03, 0xf3, 0x11, 0x03, 0xf4, 0x11, 0x03, 0xf5, 0x11, 0x03,
        0xf6, 0x11, 0x03, 0xf7, 0x11, 0x03, 0xf8, 0x11, 0x03, 0xf9, 0x11, 0x03, 0xfa, 0x11, 0x03,
        0xfb, 0x08, 0x1f, 0x22, 0x25, 0x28, 0x2b, 0x2e, 0x31, 0x34, 0x37, 0x3a, 0x3d, 0x40, 0x43,
        0x46, 0x49, 0x4c, 0x4f, 0x52, 0x55, 0x58, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x15, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5b,
    ];
    // [1, 4000000000] (mixed 1-byte + 4-byte ints)
    const BPLIST_WIDE: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xa2, 0x01, 0x02, 0x10, 0x01, 0x12, 0xee,
        0x6b, 0x28, 0x00, 0x08, 0x0b, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12,
    ];

    #[test]
    fn parses_small_int_array() {
        assert_eq!(int_array(BPLIST_THREE), Some(vec![1, 2, 3]));
    }

    #[test]
    fn parses_single_four_byte_int() {
        assert_eq!(int_array(BPLIST_SINGLE), Some(vec![123_456_789]));
    }

    #[test]
    fn parses_empty_array() {
        assert_eq!(int_array(BPLIST_EMPTY), Some(vec![]));
    }

    #[test]
    fn parses_overflow_count_array() {
        assert_eq!(
            int_array(BPLIST_BIG),
            Some((1000..=1019).collect::<Vec<i64>>())
        );
    }

    #[test]
    fn parses_mixed_width_ints() {
        assert_eq!(int_array(BPLIST_WIDE), Some(vec![1, 4_000_000_000]));
    }

    #[test]
    fn rejects_non_bplist() {
        assert_eq!(
            int_array(b"not a plist at all......................."),
            None
        );
        assert_eq!(int_array(&[]), None);
        assert_eq!(int_array(b"bplist00"), None); // header only, no trailer
    }

    #[test]
    fn rejects_truncated_offset_table() {
        // Valid header + 32-byte trailer claiming an offset table past EOF.
        let mut blob = b"bplist00".to_vec();
        blob.extend_from_slice(&[0xa0]); // empty array marker
        blob.extend(std::iter::repeat_n(0u8, 32));
        let len = blob.len();
        blob[len - 32 + 6] = 1; // offset_int_size
        blob[len - 32 + 7] = 1; // object_ref_size
        blob[len - 32 + 15] = 200; // num_objects far beyond reality
        blob[len - 32 + 31] = 9; // offset_table_offset
        assert_eq!(int_array(&blob), None);
    }

    #[test]
    fn does_not_panic_on_fuzzy_prefixes() {
        // Truncations of a valid blob must each yield None or Some, never panic.
        for cut in 0..BPLIST_THREE.len() {
            let _ = int_array(&BPLIST_THREE[..cut]);
        }
    }

    #[test]
    fn rejects_element_count_over_cap() {
        // A well-formed header/trailer whose array count (via the 0xF overflow
        // form) is 300 must be refused, not amplified. Layout: header, array
        // marker `af` + 2-byte int 300, a 1-entry offset table, 32-byte trailer.
        let blob: &[u8] = &[
            0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, // bplist00
            0xaf, 0x11, 0x01, 0x2c, // array, overflow count int = 0x012c (300)
            0x08, // offset table: object 0 at byte 8
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // trailer[0..6] unused
            0x01, // offset_int_size
            0x01, // object_ref_size
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // num_objects = 1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // top_object = 0
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, // offset_table_offset = 12
        ];
        assert_eq!(int_array(blob), None);
    }

    #[test]
    fn rejects_object_offset_into_trailer() {
        // Corrupt a valid blob so its single offset-table entry points into the
        // trailer instead of the object area. Must degrade to None.
        let mut blob = BPLIST_SINGLE.to_vec();
        let table_start = blob.len() - 32 + 24; // offset_table_offset lives here
        let table_off = read_be(&blob[table_start..table_start + 8]);
        // Point the first object offset at the trailer region.
        blob[table_off] = (blob.len() - 4) as u8;
        assert_eq!(int_array(&blob), None);
    }

    // Big-endian read helper for the corruption test above.
    fn read_be(bytes: &[u8]) -> usize {
        bytes.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize)
    }
}
