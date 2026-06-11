//! RFC-095 W2 — serde-driven 64-bit fingerprints for reactive
//! dirty-checking.
//!
//! [`fingerprint`] hashes any `Serialize` value into a `u64` by
//! streaming its serde data-model events into a
//! [`pocopine_crypto::Fnv64`] — entirely Rust-side, no allocation
//! for the common shapes, **zero wasm↔JS bridge crossings**. The
//! per-field dirty sweep (`Scope::invoke` / `Handle::update`)
//! fingerprints observed fields before and after a handler runs
//! and triggers only the fields whose fingerprints differ —
//! replacing the blanket cache-clear + scope-wide trigger sweep.
//!
//! Properties the dirty sweep relies on:
//!
//! - **Equal values ⇒ equal fingerprints.** Serialization is
//!   deterministic for a given value (struct fields in
//!   declaration order, payload bytes fixed). `HashMap` iteration
//!   order is the one wobble: same *content* can hash differently
//!   after rehashing — a false "changed", which costs a spurious
//!   re-run, never a missed one.
//! - **Different values ⇒ different fingerprints,
//!   probabilistically.** 64-bit FNV-1a; an accidental collision
//!   (a missed update) is ~2⁻⁶⁴ per write. Shapes are
//!   disambiguated with type/structure tag bytes so `1u8`,
//!   `"1"`, `[1]`, and `Some(1)` don't trivially collide.
//! - **What the fingerprint sees is what the proxy sees.** Both
//!   walk the field's `Serialize` impl, so a `#[serde(skip)]`d
//!   subfield is invisible to both — the UI can't depend on data
//!   the fingerprint misses.
//!
//! Non-goals: this is not a wire format and not stable across
//! releases — fingerprints are compared only against fingerprints
//! produced in the same process by the same build.

use core::fmt::Display;
use core::hash::Hasher as _;

use pocopine_crypto::Fnv64;
use serde::ser::{self, Serialize};

/// Fingerprint a `Serialize` value. `None` when the value's own
/// `Serialize` impl errors (the dirty sweep treats `None` as
/// "changed" — the conservative direction).
pub fn fingerprint<T: Serialize + ?Sized>(value: &T) -> Option<u64> {
    let mut h = HashSerializer(Fnv64::new());
    value.serialize(&mut h).ok()?;
    Some(h.0.finish())
}

// Type/structure tags. One byte each, written before the payload,
// so values of different serde shapes hash to different streams
// even when their payload bytes coincide.
mod tag {
    pub const BOOL: u8 = 0x01;
    pub const INT: u8 = 0x02; // all signed ints, widened to i64
    pub const UINT: u8 = 0x03; // all unsigned ints, widened to u64
    pub const I128: u8 = 0x04;
    pub const U128: u8 = 0x05;
    pub const F32: u8 = 0x06;
    pub const F64: u8 = 0x07;
    pub const CHAR: u8 = 0x08;
    pub const STR: u8 = 0x09;
    pub const BYTES: u8 = 0x0A;
    pub const NONE: u8 = 0x0B;
    pub const SOME: u8 = 0x0C;
    pub const UNIT: u8 = 0x0D;
    pub const UNIT_VARIANT: u8 = 0x0E;
    pub const NEWTYPE_VARIANT: u8 = 0x0F;
    pub const SEQ_START: u8 = 0x10;
    pub const SEQ_END: u8 = 0x11;
    pub const MAP_START: u8 = 0x12;
    pub const MAP_END: u8 = 0x13;
    pub const STRUCT_START: u8 = 0x14;
    pub const STRUCT_END: u8 = 0x15;
    pub const VARIANT_START: u8 = 0x16; // tuple/struct variants
    pub const FIELD: u8 = 0x17; // struct field name prefix
}

/// The error serde requires us to be able to represent. Our own
/// methods never fail; this only carries `T::serialize`'s custom
/// errors out to the `None` in [`fingerprint`].
#[derive(Debug)]
pub struct Unfingerprintable;

impl Display for Unfingerprintable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("value cannot be fingerprinted")
    }
}

impl std::error::Error for Unfingerprintable {}

impl ser::Error for Unfingerprintable {
    fn custom<T: Display>(_msg: T) -> Self {
        Unfingerprintable
    }
}

struct HashSerializer(Fnv64);

impl HashSerializer {
    fn put(&mut self, bytes: &[u8]) {
        self.0.write(bytes);
    }
    fn put_tag(&mut self, t: u8) {
        self.0.write(&[t]);
    }
}

type Result<T = (), E = Unfingerprintable> = core::result::Result<T, E>;

impl<'a> ser::Serializer for &'a mut HashSerializer {
    type Ok = ();
    type Error = Unfingerprintable;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    fn serialize_bool(self, v: bool) -> Result {
        self.put_tag(tag::BOOL);
        self.put(&[v as u8]);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result {
        self.serialize_i64(v.into())
    }
    fn serialize_i16(self, v: i16) -> Result {
        self.serialize_i64(v.into())
    }
    fn serialize_i32(self, v: i32) -> Result {
        self.serialize_i64(v.into())
    }
    fn serialize_i64(self, v: i64) -> Result {
        self.put_tag(tag::INT);
        self.put(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_i128(self, v: i128) -> Result {
        self.put_tag(tag::I128);
        self.put(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result {
        self.serialize_u64(v.into())
    }
    fn serialize_u16(self, v: u16) -> Result {
        self.serialize_u64(v.into())
    }
    fn serialize_u32(self, v: u32) -> Result {
        self.serialize_u64(v.into())
    }
    fn serialize_u64(self, v: u64) -> Result {
        self.put_tag(tag::UINT);
        self.put(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_u128(self, v: u128) -> Result {
        self.put_tag(tag::U128);
        self.put(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result {
        self.put_tag(tag::F32);
        self.put(&v.to_bits().to_le_bytes());
        Ok(())
    }
    fn serialize_f64(self, v: f64) -> Result {
        self.put_tag(tag::F64);
        self.put(&v.to_bits().to_le_bytes());
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result {
        self.put_tag(tag::CHAR);
        self.put(&(v as u32).to_le_bytes());
        Ok(())
    }
    fn serialize_str(self, v: &str) -> Result {
        self.put_tag(tag::STR);
        self.put(&(v.len() as u64).to_le_bytes());
        self.put(v.as_bytes());
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result {
        self.put_tag(tag::BYTES);
        self.put(&(v.len() as u64).to_le_bytes());
        self.put(v);
        Ok(())
    }
    fn serialize_none(self) -> Result {
        self.put_tag(tag::NONE);
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result {
        self.put_tag(tag::SOME);
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result {
        self.put_tag(tag::UNIT);
        Ok(())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result {
        self.serialize_unit()
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
    ) -> Result {
        self.put_tag(tag::UNIT_VARIANT);
        self.put(&variant_index.to_le_bytes());
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result {
        self.put_tag(tag::NEWTYPE_VARIANT);
        self.put(&variant_index.to_le_bytes());
        value.serialize(self)
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq> {
        self.put_tag(tag::SEQ_START);
        Ok(Compound {
            ser: self,
            end: tag::SEQ_END,
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        self.put_tag(tag::VARIANT_START);
        self.put(&variant_index.to_le_bytes());
        Ok(Compound {
            ser: self,
            end: tag::SEQ_END,
        })
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
        self.put_tag(tag::MAP_START);
        Ok(Compound {
            ser: self,
            end: tag::MAP_END,
        })
    }
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
        self.put_tag(tag::STRUCT_START);
        Ok(Compound {
            ser: self,
            end: tag::STRUCT_END,
        })
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        self.put_tag(tag::VARIANT_START);
        self.put(&variant_index.to_le_bytes());
        Ok(Compound {
            ser: self,
            end: tag::STRUCT_END,
        })
    }
    fn is_human_readable(&self) -> bool {
        // Match `serde_wasm_bindgen`'s answer so types that branch
        // on it (chrono, uuid, …) feed the fingerprint the same
        // representation the proxy serializes — "what the
        // fingerprint sees is what the proxy sees".
        true
    }
}

/// Shared compound-shape driver: seq/tuple/map/struct/variant all
/// reduce to "serialize parts in order, then write an end tag".
struct Compound<'a> {
    ser: &'a mut HashSerializer,
    end: u8,
}

impl Compound<'_> {
    fn part<T: Serialize + ?Sized>(&mut self, value: &T) -> Result {
        value.serialize(&mut *self.ser)
    }
    fn finish(self) -> Result {
        self.ser.put_tag(self.end);
        Ok(())
    }
}

impl ser::SerializeSeq for Compound<'_> {
    type Ok = ();
    type Error = Unfingerprintable;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result {
        self.part(value)
    }
    fn end(self) -> Result {
        self.finish()
    }
}

impl ser::SerializeTuple for Compound<'_> {
    type Ok = ();
    type Error = Unfingerprintable;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result {
        self.part(value)
    }
    fn end(self) -> Result {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = ();
    type Error = Unfingerprintable;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result {
        self.part(value)
    }
    fn end(self) -> Result {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = ();
    type Error = Unfingerprintable;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result {
        self.part(value)
    }
    fn end(self) -> Result {
        self.finish()
    }
}

impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Unfingerprintable;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result {
        self.part(key)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result {
        self.part(value)
    }
    fn end(self) -> Result {
        self.finish()
    }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = Unfingerprintable;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result {
        // Field names ARE discriminating: with
        // `#[serde(skip_serializing_if = ...)]` two different
        // values can emit the same number of fields in the same
        // positions — only the names tell `{a: 1}` from `{b: 1}`.
        self.ser.put_tag(tag::FIELD);
        self.ser.put(key.as_bytes());
        self.part(value)
    }
    fn end(self) -> Result {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = Unfingerprintable;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result {
        self.ser.put_tag(tag::FIELD);
        self.ser.put(key.as_bytes());
        self.part(value)
    }
    fn end(self) -> Result {
        self.finish()
    }
}

// ─── RFC-095 W2b — O(1) collection length probe ─────────────────

/// Probe a `Serialize` value's collection length WITHOUT walking
/// its contents: serde hands `serialize_seq` / `serialize_map`
/// the length *before* iterating any element, so a serializer
/// that errors out right there reads `Vec`/`HashMap`/`BTreeMap`/
/// set lengths in O(1).
///
/// The dirty sweep uses this as a short-circuit: a field whose
/// probed length moved has definitely changed — no need to hash
/// 10K structs to learn what `len()` already says. `None` means
/// "not a probeable collection" (scalars, structs, strings —
/// strings hash cheaply anyway); those take the fingerprint
/// path. Determinism: the probe is a pure function of the value,
/// so an unchanged value can never flip between `Some`/`None`
/// across the sweep — a flip IS a change (e.g. `Option<Vec<T>>`
/// going `Some(v)` → `None`).
pub fn quick_len<T: Serialize + ?Sized>(value: &T) -> Option<u64> {
    match value.serialize(LenProbe) {
        Err(LenAbort::Len(n)) => Some(n),
        _ => None,
    }
}

/// "Error" type that smuggles the length hint out of the first
/// `serialize_seq` / `serialize_map` call.
#[derive(Debug)]
enum LenAbort {
    Len(u64),
    NotCollection,
}

impl Display for LenAbort {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("len probe abort")
    }
}
impl std::error::Error for LenAbort {}
impl ser::Error for LenAbort {
    fn custom<T: Display>(_msg: T) -> Self {
        LenAbort::NotCollection
    }
}

struct LenProbe;

macro_rules! not_collection {
    ($($fn_name:ident($ty:ty);)*) => {
        $(fn $fn_name(self, _v: $ty) -> Result<(), LenAbort> {
            Err(LenAbort::NotCollection)
        })*
    };
}

impl ser::Serializer for LenProbe {
    type Ok = ();
    type Error = LenAbort;
    type SerializeSeq = ser::Impossible<(), LenAbort>;
    type SerializeTuple = ser::Impossible<(), LenAbort>;
    type SerializeTupleStruct = ser::Impossible<(), LenAbort>;
    type SerializeTupleVariant = ser::Impossible<(), LenAbort>;
    type SerializeMap = ser::Impossible<(), LenAbort>;
    type SerializeStruct = ser::Impossible<(), LenAbort>;
    type SerializeStructVariant = ser::Impossible<(), LenAbort>;

    not_collection! {
        serialize_bool(bool);
        serialize_i8(i8); serialize_i16(i16); serialize_i32(i32);
        serialize_i64(i64); serialize_i128(i128);
        serialize_u8(u8); serialize_u16(u16); serialize_u32(u32);
        serialize_u64(u64); serialize_u128(u128);
        serialize_f32(f32); serialize_f64(f64);
        serialize_char(char);
        serialize_str(&str);
        serialize_bytes(&[u8]);
    }

    fn serialize_none(self) -> Result<(), LenAbort> {
        Err(LenAbort::NotCollection)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), LenAbort> {
        // Transparent: probe the inner value so `Option<Vec<T>>`
        // short-circuits too. `Some(v)` vs `None` flips the probe
        // result, which the sweep reads as changed — correct.
        value.serialize(LenProbe)
    }
    fn serialize_unit(self) -> Result<(), LenAbort> {
        Err(LenAbort::NotCollection)
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), LenAbort> {
        Err(LenAbort::NotCollection)
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<(), LenAbort> {
        Err(LenAbort::NotCollection)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), LenAbort> {
        value.serialize(LenProbe)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<(), LenAbort> {
        // Variant identity isn't captured by a bare length —
        // leave enum payloads to the fingerprint.
        Err(LenAbort::NotCollection)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, LenAbort> {
        Err(match len {
            Some(n) => LenAbort::Len(n as u64),
            None => LenAbort::NotCollection,
        })
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, LenAbort> {
        // Tuples are fixed-arity — length can never change, so a
        // probe would always say "equal" and add nothing.
        Err(LenAbort::NotCollection)
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, LenAbort> {
        Err(LenAbort::NotCollection)
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, LenAbort> {
        Err(LenAbort::NotCollection)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, LenAbort> {
        Err(match len {
            Some(n) => LenAbort::Len(n as u64),
            None => LenAbort::NotCollection,
        })
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, LenAbort> {
        Err(LenAbort::NotCollection)
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, LenAbort> {
        Err(LenAbort::NotCollection)
    }
    fn is_human_readable(&self) -> bool {
        true
    }
}

// ─── RFC-096 S3 — typed text projection ─────────────────────────

/// Serialize a scalar value straight to the string `pp-text`
/// would render — `Some` only for serde-scalar shapes (str,
/// numbers, bool, char, unit/`None`, transparently-wrapped
/// versions of those), `None` for compound shapes (which take
/// the JsValue-projection path). Output parity is with the
/// runtime's `js_to_string(serde_wasm_bindgen::to_value(v))`:
/// integral floats drop the `.0`, `None`/unit render as `""`.
pub fn text_projection<T: Serialize + ?Sized>(value: &T) -> Option<String> {
    value.serialize(TextProjector).ok()
}

struct NotScalar;

impl Display for NotScalar {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("not a scalar")
    }
}
impl core::fmt::Debug for NotScalar {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("NotScalar")
    }
}
impl std::error::Error for NotScalar {}
impl ser::Error for NotScalar {
    fn custom<T: Display>(_msg: T) -> Self {
        NotScalar
    }
}

struct TextProjector;

fn float_text(n: f64) -> String {
    // Parity with the runtime js_to_string copies — the shared
    // helper has JS String() semantics on wasm and a saturation-
    // safe Display approximation on the host.
    crate::text::js_number_string(n)
}

impl ser::Serializer for TextProjector {
    type Ok = String;
    type Error = NotScalar;
    type SerializeSeq = ser::Impossible<String, NotScalar>;
    type SerializeTuple = ser::Impossible<String, NotScalar>;
    type SerializeTupleStruct = ser::Impossible<String, NotScalar>;
    type SerializeTupleVariant = ser::Impossible<String, NotScalar>;
    type SerializeMap = ser::Impossible<String, NotScalar>;
    type SerializeStruct = ser::Impossible<String, NotScalar>;
    type SerializeStructVariant = ser::Impossible<String, NotScalar>;

    fn serialize_bool(self, v: bool) -> Result<String, NotScalar> {
        Ok(if v { "true".into() } else { "false".into() })
    }
    fn serialize_i8(self, v: i8) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_i16(self, v: i16) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_i32(self, v: i32) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_i64(self, v: i64) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_i128(self, v: i128) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_u8(self, v: u8) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_u16(self, v: u16) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_u32(self, v: u32) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_u64(self, v: u64) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_u128(self, v: u128) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_f32(self, v: f32) -> Result<String, NotScalar> {
        Ok(float_text(v as f64))
    }
    fn serialize_f64(self, v: f64) -> Result<String, NotScalar> {
        Ok(float_text(v))
    }
    fn serialize_char(self, v: char) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_str(self, v: &str) -> Result<String, NotScalar> {
        Ok(v.to_string())
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<String, NotScalar> {
        Err(NotScalar)
    }
    fn serialize_none(self) -> Result<String, NotScalar> {
        // `None` serializes to JS `null`; js_to_string(null) = "".
        Ok(String::new())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<String, NotScalar> {
        value.serialize(TextProjector)
    }
    fn serialize_unit(self) -> Result<String, NotScalar> {
        Ok(String::new())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<String, NotScalar> {
        Ok(String::new())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<String, NotScalar> {
        // Externally-tagged unit variant serializes to its name
        // string; js_to_string renders it as-is.
        Ok(variant.to_string())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<String, NotScalar> {
        value.serialize(TextProjector)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<String, NotScalar> {
        Err(NotScalar)
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, NotScalar> {
        Err(NotScalar)
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, NotScalar> {
        Err(NotScalar)
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, NotScalar> {
        Err(NotScalar)
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, NotScalar> {
        Err(NotScalar)
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, NotScalar> {
        Err(NotScalar)
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, NotScalar> {
        Err(NotScalar)
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, NotScalar> {
        Err(NotScalar)
    }
    fn is_human_readable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::fingerprint;
    use serde::Serialize;

    fn fp<T: Serialize>(v: T) -> u64 {
        fingerprint(&v).expect("fingerprintable")
    }

    #[test]
    fn equal_values_equal_fingerprints() {
        assert_eq!(fp(42u32), fp(42u32));
        assert_eq!(fp("hello".to_string()), fp("hello".to_string()));
        assert_eq!(fp(vec![1u8, 2, 3]), fp(vec![1u8, 2, 3]));
        assert_eq!(fp(Some(1.5f64)), fp(Some(1.5f64)));
        // NaN: bit-identical NaNs fingerprint equal (no trigger
        // when overwriting NaN with the same NaN — correct, the
        // proxy value didn't observably change either).
        assert_eq!(fp(f64::NAN), fp(f64::NAN));
    }

    #[test]
    fn changed_values_change_fingerprints() {
        assert_ne!(fp(42u32), fp(43u32));
        assert_ne!(fp("a"), fp("b"));
        assert_ne!(fp(vec![1u8, 2]), fp(vec![2u8, 1]));
        assert_ne!(fp(Some(0u8)), fp(None::<u8>));
        assert_ne!(fp(0.0f64), fp(-0.0f64)); // sign bit is observable via JS
    }

    #[test]
    fn shape_tags_prevent_cross_type_collisions() {
        // Same payload bytes, different serde shapes.
        assert_ne!(fp(1u64), fp(1i64));
        assert_ne!(fp("1"), fp(1u64));
        assert_ne!(fp(vec![1u64]), fp(1u64));
        assert_ne!(fp(Some(1u64)), fp(1u64));
        assert_ne!(fp(()), fp(false));
    }

    #[test]
    fn nesting_boundaries_are_delimited() {
        // [[1],[2]] vs [[1,2]] — flattening must not collide.
        assert_ne!(fp(vec![vec![1u8], vec![2u8]]), fp(vec![vec![1u8, 2u8]]));
        // ("ab","c") vs ("a","bc") — length prefixes prevent
        // string-concat collisions.
        assert_ne!(fp(("ab", "c")), fp(("a", "bc")));
    }

    #[test]
    fn structs_and_enums_discriminate() {
        #[derive(Serialize)]
        struct User {
            name: String,
            age: u32,
        }
        #[derive(Serialize)]
        enum Status {
            Idle,
            Loading,
            Ready { count: u32 },
            Error(String),
        }

        let a = User {
            name: "ana".into(),
            age: 30,
        };
        let b = User {
            name: "ana".into(),
            age: 31,
        };
        assert_eq!(
            fp(User {
                name: "ana".into(),
                age: 30
            }),
            fp(User {
                name: "ana".into(),
                age: 30
            })
        );
        assert_ne!(fp(a), fp(b));

        assert_ne!(fp(Status::Idle), fp(Status::Loading));
        assert_ne!(
            fp(Status::Ready { count: 1 }),
            fp(Status::Ready { count: 2 })
        );
        assert_ne!(
            fp(Status::Error("x".into())),
            fp(Status::Ready { count: 0 })
        );
        assert_eq!(fp(Status::Error("x".into())), fp(Status::Error("x".into())));
    }

    #[test]
    fn quick_len_probes_collections_in_o1() {
        use super::quick_len as ql;
        use std::collections::HashMap;
        assert_eq!(ql(&vec![1u8, 2, 3]), Some(3));
        assert_eq!(ql(&Vec::<u8>::new()), Some(0));
        let mut m = HashMap::new();
        m.insert("a", 1u8);
        assert_eq!(ql(&m), Some(1));
        assert_eq!(ql(&Some(vec![1u8])), Some(1));
        // Non-collections refuse.
        assert_eq!(ql(&42u32), None);
        assert_eq!(ql("hello"), None);
        assert_eq!(ql(&None::<Vec<u8>>), None);
        assert_eq!(ql(&(1u8, 2u8)), None);
        #[derive(Serialize)]
        struct S {
            a: u8,
        }
        assert_eq!(ql(&S { a: 1 }), None);
    }

    #[test]
    fn text_projection_matches_js_to_string_semantics() {
        use super::text_projection as tp;
        assert_eq!(tp(&42u32), Some("42".into()));
        assert_eq!(tp(&-7i64), Some("-7".into()));
        assert_eq!(tp(&1.5f64), Some("1.5".into()));
        assert_eq!(tp(&2.0f64), Some("2".into()));
        assert_eq!(tp(&true), Some("true".into()));
        assert_eq!(tp("hello"), Some("hello".into()));
        assert_eq!(tp(&'x'), Some("x".into()));
        assert_eq!(tp(&None::<u32>), Some(String::new()));
        assert_eq!(tp(&Some(5u8)), Some("5".into()));
        assert_eq!(tp(&()), Some(String::new()));
        // Compound shapes refuse — they take the projection path.
        assert_eq!(tp(&vec![1u8]), None);
        #[derive(Serialize)]
        struct S {
            a: u8,
        }
        assert_eq!(tp(&S { a: 1 }), None);
    }

    #[test]
    fn vec_of_structs_detects_single_element_change() {
        #[derive(Serialize, Clone)]
        struct Row {
            id: u32,
            label: String,
        }
        let rows: Vec<Row> = (0..100)
            .map(|i| Row {
                id: i,
                label: format!("row {i}"),
            })
            .collect();
        let mut changed = rows.clone();
        changed[57].label = "row 57 !!!".into();
        assert_eq!(fp(rows.clone()), fp(rows.clone()));
        assert_ne!(fp(rows), fp(changed));
    }
}
