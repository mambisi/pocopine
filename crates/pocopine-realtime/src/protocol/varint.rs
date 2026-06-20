//! lib0 `varUint` codec.
//!
//! RFC 073 §7 specifies the frame envelope uses lib0 `varUint` (LEB128 of an
//! unsigned integer) — the same encoding the Yjs collab payloads use
//! internally, so the envelope and the application payload can share one
//! decoder. This module is the shared decoder.

use super::error::ProtocolError;

/// Maximum bytes a `u64` LEB128 value may occupy (ceil(64 / 7) == 10).
const MAX_VARUINT_BYTES: usize = 10;

/// Append `value` to `buf` as a lib0 `varUint` (LEB128, little-endian groups
/// of 7 bits, high bit = "more bytes follow").
pub fn write_var_uint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

/// Read a lib0 `varUint` starting at `*pos`, advancing `*pos` past it.
///
/// Errors if the buffer ends mid-value or the value would overflow `u64`.
pub fn read_var_uint(buf: &[u8], pos: &mut usize) -> Result<u64, ProtocolError> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    let mut read = 0usize;

    loop {
        let byte = *buf
            .get(*pos)
            .ok_or_else(|| ProtocolError::frame("varuint: unexpected end of buffer"))?;
        *pos += 1;
        read += 1;

        // The final (10th) byte of a u64 may only carry the top bit; reject
        // anything that would shift past 64 bits.
        if shift >= 64 || read > MAX_VARUINT_BYTES {
            return Err(ProtocolError::frame("varuint: value overflows u64"));
        }
        result |= u64::from(byte & 0x7f)
            .checked_shl(shift)
            .ok_or_else(|| ProtocolError::frame("varuint: value overflows u64"))?;

        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: u64) {
        let mut buf = Vec::new();
        write_var_uint(&mut buf, value);
        let mut pos = 0;
        let decoded = read_var_uint(&buf, &mut pos).expect("decode");
        assert_eq!(decoded, value, "value {value} did not round-trip");
        assert_eq!(
            pos,
            buf.len(),
            "decoder consumed wrong byte count for {value}"
        );
    }

    #[test]
    fn roundtrips_boundary_values() {
        for value in [
            0,
            1,
            0x7f,
            0x80,
            0x3fff,
            0x4000,
            300,
            16_384,
            u32::MAX as u64,
            u64::MAX - 1,
            u64::MAX,
        ] {
            roundtrip(value);
        }
    }

    #[test]
    fn single_byte_for_small_values() {
        let mut buf = Vec::new();
        write_var_uint(&mut buf, 0x7f);
        assert_eq!(buf, vec![0x7f]);

        buf.clear();
        write_var_uint(&mut buf, 0x80);
        assert_eq!(buf, vec![0x80, 0x01]);
    }

    #[test]
    fn truncated_buffer_errors() {
        // High bit set but no following byte.
        let buf = [0x80u8];
        let mut pos = 0;
        assert!(read_var_uint(&buf, &mut pos).is_err());
    }

    #[test]
    fn overlong_buffer_errors() {
        // 11 continuation bytes — would overflow u64.
        let buf = [0xffu8; 11];
        let mut pos = 0;
        assert!(read_var_uint(&buf, &mut pos).is_err());
    }

    #[test]
    fn reads_sequential_values() {
        let mut buf = Vec::new();
        write_var_uint(&mut buf, 3);
        write_var_uint(&mut buf, 300);
        write_var_uint(&mut buf, 1);
        let mut pos = 0;
        assert_eq!(read_var_uint(&buf, &mut pos).unwrap(), 3);
        assert_eq!(read_var_uint(&buf, &mut pos).unwrap(), 300);
        assert_eq!(read_var_uint(&buf, &mut pos).unwrap(), 1);
        assert_eq!(pos, buf.len());
    }
}
