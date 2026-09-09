//! Little-endian cursor over one export's bytes, with the UE string and FName primitives.

use retoc::legacy_asset::{FMinimalName, FPackageNameMap};

pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    /// Offset of `data[0]` inside the containing file, so errors report real file positions.
    base: u64,
    /// Where every FName was read from. Copying an export into another package has to rewrite
    /// each of them against that package's name map, and there is nothing in the bytes that says
    /// which four-byte pairs are names.
    names: Vec<u64>,
}

macro_rules! read_int {
    ($name:ident, $ty:ty) => {
        pub(crate) fn $name(&mut self) -> Result<$ty, String> {
            const N: usize = size_of::<$ty>();
            let bytes = self.take(N)?;
            let mut buf = [0u8; N];
            buf.copy_from_slice(bytes);
            Ok(<$ty>::from_le_bytes(buf))
        }
    };
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(data: &'a [u8], base: u64) -> Self {
        Self {
            data,
            pos: 0,
            base,
            names: Vec::new(),
        }
    }

    /// The offsets of every FName read so far, taken out of the cursor.
    pub(crate) fn take_names(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.names)
    }

    pub(crate) fn names_len(&self) -> usize {
        self.names.len()
    }

    /// Forgets the names read since a mark, for a parse that rolled back over them.
    pub(crate) fn truncate_names(&mut self, to: usize) {
        self.names.truncate(to);
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn file_offset(&self) -> u64 {
        self.base + self.pos as u64
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// The bytes around the cursor, plus the index within that slice where the cursor sits.
    pub(crate) fn window(&self, context: usize) -> (&'a [u8], usize) {
        let start = self.pos.saturating_sub(context);
        let marker = self.pos - start;
        (&self.data[start.min(self.data.len())..], marker)
    }

    /// The bytes between two positions this cursor has already passed.
    pub(crate) fn slice(&self, from: usize, to: usize) -> &'a [u8] {
        let end = to.min(self.data.len());
        &self.data[from.min(end)..end]
    }

    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }

    pub(crate) fn err(&self, message: impl AsRef<str>) -> String {
        format!("{} at offset 0x{:X}", message.as_ref(), self.file_offset())
    }

    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or_else(|| self.err("length overflow"))?;
        if end > self.data.len() {
            return Err(self.err(format!(
                "wanted {count} bytes but only {} remain",
                self.remaining()
            )));
        }
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    /// Places the cursor at an absolute offset within this export, for containers that record
    /// the byte length of their payload and can therefore resynchronise.
    pub(crate) fn seek_to(&mut self, position: usize) -> Result<(), String> {
        if position > self.data.len() {
            return Err(self.err(format!(
                "cannot seek to {position} in a {} byte export",
                self.data.len()
            )));
        }
        self.pos = position;
        Ok(())
    }

    pub(crate) fn skip(&mut self, count: usize) -> Result<(), String> {
        self.take(count).map(|_| ())
    }

    read_int!(read_u8, u8);
    read_int!(read_i8, i8);
    read_int!(read_u16, u16);
    read_int!(read_i16, i16);
    read_int!(read_u32, u32);
    read_int!(read_i32, i32);
    read_int!(read_u64, u64);
    read_int!(read_i64, i64);

    /// Reads a 32-bit word without advancing, for deciding whether a trailing word is a flag.
    /// The next byte without consuming it, for a list that runs until its terminator token.
    pub(crate) fn peek_u8(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub(crate) fn peek_u32(&self) -> Option<u32> {
        let bytes = self.data.get(self.pos..self.pos + 4)?;
        let mut buf = [0u8; 4];
        buf.copy_from_slice(bytes);
        Some(u32::from_le_bytes(buf))
    }

    pub(crate) fn read_f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    pub(crate) fn read_f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    /// UE serializes a 32-bit boolean as a full word in most struct layouts.
    pub(crate) fn read_bool32(&mut self) -> Result<bool, String> {
        Ok(self.read_u32()? != 0)
    }

    /// Negative lengths mean UTF-16, positive mean ANSI. Both counts include the null terminator.
    pub(crate) fn read_string(&mut self) -> Result<String, String> {
        let len = self.read_i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len < 0 {
            let count = len.unsigned_abs() as usize;
            if count.saturating_mul(2) > self.remaining() {
                return Err(self.err(format!(
                    "utf-16 string of {count} chars overruns the export"
                )));
            }
            let mut units = Vec::with_capacity(count);
            for _ in 0..count {
                units.push(self.read_u16()?);
            }
            while units.last() == Some(&0) {
                units.pop();
            }
            String::from_utf16(&units).map_err(|e| self.err(format!("invalid utf-16 string: {e}")))
        } else {
            let bytes = self.take(len as usize)?;
            let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
            Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
        }
    }

    pub(crate) fn read_name(&mut self, names: &FPackageNameMap) -> Result<String, String> {
        let at = self.file_offset();
        let index = self.read_i32()?;
        let number = self.read_i32()?;
        self.names.push(at);
        names
            .get(FMinimalName { index, number })
            .map(|name| name.into_owned())
            .map_err(|e| self.err(format!("resolve FName: {e}")))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reading_past_the_end_reports_the_file_offset_rather_than_the_slice_offset() {
        let data = [1u8, 2, 3];
        let mut cursor = Cursor::new(&data, 0x1000);
        cursor.skip(2).expect("skip");
        let err = cursor.read_u32().expect_err("should overrun");
        assert!(err.contains("0x1002"), "{err}");
    }

    #[test]
    fn an_ansi_string_drops_its_null_terminator() {
        let mut data = 5i32.to_le_bytes().to_vec();
        data.extend_from_slice(b"Hero\0");
        let mut cursor = Cursor::new(&data, 0);
        assert_eq!(cursor.read_string().expect("string"), "Hero");
        assert!(cursor.remaining() == 0);
    }

    #[test]
    fn a_negative_length_string_is_read_as_utf16() {
        let mut data = (-3i32).to_le_bytes().to_vec();
        for unit in [0x00E9u16, 0x0061, 0x0000] {
            data.extend_from_slice(&unit.to_le_bytes());
        }
        let mut cursor = Cursor::new(&data, 0);
        assert_eq!(cursor.read_string().expect("string"), "\u{e9}a");
    }

    #[test]
    fn an_empty_string_consumes_only_its_length() {
        let data = 0i32.to_le_bytes();
        let mut cursor = Cursor::new(&data, 0);
        assert_eq!(cursor.read_string().expect("string"), "");
        assert!(cursor.remaining() == 0);
    }
}
