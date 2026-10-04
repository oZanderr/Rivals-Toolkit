//! Decodes the UE 4.25+ unversioned property header: runs of skipped and present schema slots,
//! plus the bit mask naming the values that were all zero and therefore never written.

use crate::reader::Cursor;

const SKIP_NUM_MASK: u16 = 0x007F;
const HAS_ZEROES_MASK: u16 = 0x0080;
const IS_LAST_MASK: u16 = 0x0100;
const VALUE_NUM_SHIFT: u16 = 9;
/// Both counts in a fragment are seven bits wide.
const FRAGMENT_CAP: u32 = 127;

/// One property present in the stream, identified by its index into the flattened schema.
#[derive(Debug, Clone)]
pub(crate) struct HeaderItem {
    pub schema_index: u32,
    /// The value was all zero, so it occupies no bytes and holds its default.
    pub is_zero: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fragment {
    skip: u32,
    has_zeroes: bool,
    value_num: u32,
    is_last: bool,
}

/// A header as it was written, not merely what it decoded to.
///
/// Every header the game ships is the one UE's header builder writes for the values it holds, which
/// an edit rebuilds the same way (see `normalize`). A header written by something else can take
/// other shapes; keeping the fragments as read lets one nothing edits re-emit byte for byte.
#[derive(Debug, Clone)]
pub(crate) struct UnversionedHeader {
    fragments: Vec<Fragment>,
    pub items: Vec<HeaderItem>,
    /// One bit per value belonging to a fragment that declares zeroes, in stream order.
    zero_bits: Vec<bool>,
    /// The mask exactly as it was read. The bits past the last value are undefined, and a header
    /// written by something other than the game may have set them.
    zero_mask_bytes: Vec<u8>,
}

impl UnversionedHeader {
    /// Moves every slot at or above `from` up by `by`, as `by` slots put in at `from` leave them.
    /// The values keep their order, so the bytes after the header stay as they are.
    pub(crate) fn shift_from(&mut self, from: u32, by: u32) {
        for item in &mut self.items {
            if item.schema_index >= from {
                item.schema_index += by;
            }
        }
        self.normalize();
    }

    /// How many schema slots the fragments describe, stored or skipped. A header written for the
    /// struct this build ships never reaches past the struct's own slot count.
    pub(crate) fn covered_slots(&self) -> usize {
        self.fragments
            .iter()
            .map(|fragment| (fragment.skip + fragment.value_num) as usize)
            .sum()
    }
}

/// Where a schema slot falls among the fragments.
enum Placement {
    /// Inside a fragment's skip run, `offset` slots into it.
    Skipped {
        fragment: usize,
        offset: u32,
        /// Zero bits spent by the fragments before this one.
        bit: usize,
    },
    /// One of a fragment's values, `offset` values into it.
    Present {
        fragment: usize,
        offset: u32,
        bit: usize,
    },
    /// Past everything the fragments describe; `covered` is the first slot they do not reach.
    Beyond { covered: u32, bit: usize },
}

fn unpack(packed: u16) -> Fragment {
    Fragment {
        skip: u32::from(packed & SKIP_NUM_MASK),
        has_zeroes: packed & HAS_ZEROES_MASK != 0,
        value_num: u32::from(packed >> VALUE_NUM_SHIFT),
        is_last: packed & IS_LAST_MASK != 0,
    }
}

fn pack(fragment: &Fragment) -> Result<u16, String> {
    if fragment.skip > FRAGMENT_CAP || fragment.value_num > FRAGMENT_CAP {
        return Err(format!(
            "a header fragment cannot skip {} slots or hold {} values; both stop at {FRAGMENT_CAP}",
            fragment.skip, fragment.value_num
        ));
    }
    Ok((fragment.skip as u16 & SKIP_NUM_MASK)
        | if fragment.has_zeroes {
            HAS_ZEROES_MASK
        } else {
            0
        }
        | if fragment.is_last { IS_LAST_MASK } else { 0 }
        | ((fragment.value_num as u16) << VALUE_NUM_SHIFT))
}

/// A fragment run is bounded by the 7-bit skip and value fields, so a header cannot legitimately
/// exceed this many fragments for any real struct.
const MAX_FRAGMENTS: usize = 8192;

/// A header holding values that ends on a fragment with no values.
pub(crate) const SHAPE_ENDS_ON_SKIP: &str = "ends on a skip";
/// A fragment that declares zeroes where none of its values is zero.
pub(crate) const SHAPE_ZERO_FLAG_WITHOUT_ZERO: &str = "zero flag with no zero under it";
/// A fragment with no values that declares zeroes.
pub(crate) const SHAPE_ZERO_FLAG_ON_NOTHING: &str = "zero flag on a fragment with no values";
/// A header holding no values written as more than one fragment.
pub(crate) const SHAPE_EMPTY_OVER_FRAGMENTS: &str = "no values over several fragments";
/// A fragment skipping and holding nothing, before the last.
pub(crate) const SHAPE_EMPTY_FRAGMENT: &str = "empty fragment before the last";
/// Bits set in the zero mask past the last one a value owns.
pub(crate) const SHAPE_DIRTY_MASK_TAIL: &str = "bits set past the end of the zero mask";
/// A fragment that only skips, before the last.
pub(crate) const SHAPE_SKIP_BETWEEN: &str = "skip-only fragment before the last";
/// A run of values split across two fragments with no skip between them.
pub(crate) const SHAPE_SPLIT_RUN: &str = "run of values split with no skip";
/// Fragments or a zero mask other than the ones UE's header builder writes for the same values.
pub(crate) const SHAPE_NOT_BUILT: &str = "not the header UE builds from its values";

/// The shapes no header the game ships takes, found by counting every shape over 8.9 million of its
/// headers: a save that writes one writes something UE never would. A skip-only fragment before the
/// last is the one shape they do take, where a run of skips reaches 127.
pub(crate) const NEVER_WRITTEN: &[&str] = &[
    SHAPE_ENDS_ON_SKIP,
    SHAPE_ZERO_FLAG_WITHOUT_ZERO,
    SHAPE_ZERO_FLAG_ON_NOTHING,
    SHAPE_EMPTY_OVER_FRAGMENTS,
    SHAPE_EMPTY_FRAGMENT,
    SHAPE_DIRTY_MASK_TAIL,
    SHAPE_SPLIT_RUN,
    SHAPE_NOT_BUILT,
];

/// The fragments and zero mask UE's header builder writes for these values, which must not be empty.
fn built(items: &[HeaderItem]) -> (Vec<Fragment>, Vec<bool>) {
    let fresh = || Fragment {
        skip: 0,
        has_zeroes: false,
        value_num: 0,
        is_last: false,
    };
    // A fragment that turned out to hold no zero gives its bits back.
    let close = |fragment: &Fragment, mask: &mut Vec<bool>| {
        if !fragment.has_zeroes {
            mask.truncate(mask.len() - fragment.value_num as usize);
        }
    };
    let mut fragments = vec![fresh()];
    let mut mask: Vec<bool> = Vec::new();
    let mut slot = 0u32;
    for item in items {
        while slot < item.schema_index {
            let last = fragments.last().copied().unwrap_or_else(fresh);
            if last.value_num > 0 || last.skip == FRAGMENT_CAP {
                close(&last, &mut mask);
                fragments.push(fresh());
            }
            if let Some(last) = fragments.last_mut() {
                last.skip += 1;
            }
            slot += 1;
        }
        let last = fragments.last().copied().unwrap_or_else(fresh);
        if last.value_num == FRAGMENT_CAP {
            close(&last, &mut mask);
            fragments.push(fresh());
        }
        if let Some(last) = fragments.last_mut() {
            last.value_num += 1;
            last.has_zeroes |= item.is_zero;
        }
        mask.push(item.is_zero);
        slot += 1;
    }
    if let Some(last) = fragments.last() {
        close(last, &mut mask);
    }
    if let Some(last) = fragments.last_mut() {
        last.is_last = true;
    }
    (fragments, mask)
}

fn ends_on_skip(fragments: &[Fragment]) -> bool {
    fragments.last().is_some_and(|last| last.value_num == 0)
        && fragments.iter().any(|fragment| fragment.value_num > 0)
}

pub(crate) fn read_header(cursor: &mut Cursor<'_>) -> Result<UnversionedHeader, String> {
    let mut fragments = Vec::new();
    loop {
        let fragment = unpack(cursor.read_u16()?);
        let is_last = fragment.is_last;
        fragments.push(fragment);
        if is_last {
            break;
        }
        if fragments.len() >= MAX_FRAGMENTS {
            return Err(cursor.err("unversioned header has no terminating fragment"));
        }
    }

    // UE's loader steps past a fragment with no values to reach the next, so one ending a header
    // that holds values sends it past the fragments and the game crashes. Nothing the game ships
    // has one; a package that does was written wrong.
    if ends_on_skip(&fragments) {
        return Err(cursor.err(
            "unversioned header ends on a fragment with no values, which the game's loader reads past",
        ));
    }
    let zero_bits: u32 = fragments
        .iter()
        .filter(|f| f.has_zeroes)
        .map(|f| f.value_num)
        .sum();
    let mask_at = cursor.position();
    let zero_mask = read_zero_mask(cursor, zero_bits)?;
    let zero_mask_bytes = cursor.slice(mask_at, cursor.position()).to_vec();

    let mut items = Vec::new();
    let mut schema_index: u32 = 0;
    let mut zero_cursor: usize = 0;
    for fragment in &fragments {
        schema_index = schema_index
            .checked_add(fragment.skip)
            .ok_or_else(|| cursor.err("schema index overflow"))?;
        for _ in 0..fragment.value_num {
            let is_zero =
                fragment.has_zeroes && zero_mask.get(zero_cursor).copied().unwrap_or(false);
            if fragment.has_zeroes {
                zero_cursor += 1;
            }
            items.push(HeaderItem {
                schema_index,
                is_zero,
            });
            schema_index = schema_index
                .checked_add(1)
                .ok_or_else(|| cursor.err("schema index overflow"))?;
        }
    }
    Ok(UnversionedHeader {
        fragments,
        items,
        zero_bits: zero_mask,
        zero_mask_bytes,
    })
}

impl UnversionedHeader {
    /// Whether the header holds values and ends on a fragment that only skips, which the game's
    /// loader reads past.
    pub(crate) fn ends_on_skip(&self) -> bool {
        ends_on_skip(&self.fragments)
    }

    /// Which of the shapes the audit counts this header takes.
    pub(crate) fn shapes(&self) -> Vec<&'static str> {
        let mut shapes = Vec::new();
        if self.ends_on_skip() {
            shapes.push(SHAPE_ENDS_ON_SKIP);
        }
        let mut bit = 0usize;
        let (mut without_zero, mut on_nothing) = (false, false);
        for fragment in &self.fragments {
            if !fragment.has_zeroes {
                continue;
            }
            let count = fragment.value_num as usize;
            if count == 0 {
                on_nothing = true;
            } else if !self
                .zero_bits
                .get(bit..bit + count)
                .is_some_and(|bits| bits.iter().any(|zero| *zero))
            {
                without_zero = true;
            }
            bit += count;
        }
        if without_zero {
            shapes.push(SHAPE_ZERO_FLAG_WITHOUT_ZERO);
        }
        if on_nothing {
            shapes.push(SHAPE_ZERO_FLAG_ON_NOTHING);
        }
        if self.items.is_empty() && self.fragments.len() > 1 {
            shapes.push(SHAPE_EMPTY_OVER_FRAGMENTS);
        }
        if self.fragments[..self.fragments.len().saturating_sub(1)]
            .iter()
            .any(|fragment| fragment.skip == 0 && fragment.value_num == 0)
        {
            shapes.push(SHAPE_EMPTY_FRAGMENT);
        }
        let before_last = &self.fragments[..self.fragments.len().saturating_sub(1)];
        if before_last
            .iter()
            .any(|fragment| fragment.skip > 0 && fragment.value_num == 0)
        {
            shapes.push(SHAPE_SKIP_BETWEEN);
        }
        if !self.items.is_empty()
            && (self.fragments.clone(), self.zero_bits.clone()) != built(&self.items)
        {
            shapes.push(SHAPE_NOT_BUILT);
        }
        if self.fragments.windows(2).any(|pair| {
            pair[0].value_num > 0
                && pair[0].value_num < FRAGMENT_CAP
                && pair[1].skip == 0
                && pair[1].value_num > 0
        }) {
            shapes.push(SHAPE_SPLIT_RUN);
        }
        let tail_set = self
            .zero_mask_bytes
            .iter()
            .enumerate()
            .flat_map(|(at, byte)| (0..8).map(move |bit| (at * 8 + bit, byte >> bit & 1 == 1)))
            .any(|(index, set)| set && index >= self.zero_bits.len());
        if tail_set {
            shapes.push(SHAPE_DIRTY_MASK_TAIL);
        }
        shapes
    }

    /// Re-emits the header. Identical to the bytes it was read from unless an edit changed it.
    pub(crate) fn write(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(self.fragments.len() * 2 + 4);
        for fragment in &self.fragments {
            out.extend_from_slice(&pack(fragment)?.to_le_bytes());
        }
        write_zero_mask(&mut out, &self.zero_bits, &self.zero_mask_bytes);
        Ok(out)
    }

    /// Whether the header carries this slot at all, as a value or as a zero flag.
    pub(crate) fn has(&self, schema_index: u32) -> bool {
        self.ordinal(schema_index).is_some()
    }

    /// The position of a stored slot among the items, which are dense and in schema order.
    fn ordinal(&self, schema_index: u32) -> Option<usize> {
        self.items
            .binary_search_by_key(&schema_index, |item| item.schema_index)
            .ok()
    }

    fn place(&self, schema_index: u32) -> Placement {
        let mut slot = 0u32;
        let mut bit = 0usize;
        for (index, fragment) in self.fragments.iter().enumerate() {
            let values_start = slot.saturating_add(fragment.skip);
            if schema_index < values_start {
                return Placement::Skipped {
                    fragment: index,
                    offset: schema_index - slot,
                    bit,
                };
            }
            if schema_index < values_start.saturating_add(fragment.value_num) {
                return Placement::Present {
                    fragment: index,
                    offset: schema_index - values_start,
                    bit,
                };
            }
            slot = values_start.saturating_add(fragment.value_num);
            if fragment.has_zeroes {
                bit += fragment.value_num as usize;
            }
        }
        Placement::Beyond { covered: slot, bit }
    }

    /// Where the mask bit for `item` lives, or `None` when the fragment holding it declares no
    /// zeroes and so spends no bits on its values.
    fn zero_bit_of(&self, item: usize) -> Option<usize> {
        let mut value = 0usize;
        let mut bit = 0usize;
        for fragment in &self.fragments {
            let count = fragment.value_num as usize;
            if item < value + count {
                return fragment.has_zeroes.then(|| bit + (item - value));
            }
            value += count;
            if fragment.has_zeroes {
                bit += count;
            }
        }
        None
    }

    /// Marks a slot's value as stored, so its bytes are read rather than assumed. The fragment
    /// already spends a bit on it, so the header keeps its exact length.
    pub(crate) fn store(&mut self, schema_index: u32) -> Result<(), String> {
        let item = self
            .ordinal(schema_index)
            .ok_or("this property is not in the header")?;
        let bit = self
            .zero_bit_of(item)
            .ok_or("this property is already stored")?;
        *self
            .zero_bits
            .get_mut(bit)
            .ok_or("the zero mask is shorter than its fragments claim")? = false;
        if let Some(entry) = self.items.get_mut(item) {
            entry.is_zero = false;
        }
        self.normalize();
        Ok(())
    }

    /// Marks a slot's value as all zero, so its bytes are dropped. The fragment holding it may have
    /// to start declaring zeroes, which lengthens the mask.
    pub(crate) fn clear(&mut self, schema_index: u32) -> Result<(), String> {
        let item = self
            .ordinal(schema_index)
            .ok_or("this property is not in the header")?;
        if self.zero_bit_of(item).is_none() {
            self.declare_zeroes(item)?;
        }
        let bit = self
            .zero_bit_of(item)
            .ok_or("the fragment holding this property does not carry a zero mask")?;
        *self
            .zero_bits
            .get_mut(bit)
            .ok_or("the zero mask is shorter than its fragments claim")? = true;
        if let Some(entry) = self.items.get_mut(item) {
            entry.is_zero = true;
        }
        self.normalize();
        Ok(())
    }

    /// Gives the fragment holding `item` a mask, inserting a cleared bit for each of its values.
    fn declare_zeroes(&mut self, item: usize) -> Result<(), String> {
        let mut value = 0usize;
        let mut bit = 0usize;
        for fragment in &mut self.fragments {
            let count = fragment.value_num as usize;
            if item < value + count {
                fragment.has_zeroes = true;
                for offset in 0..count {
                    self.zero_bits.insert(bit + offset, false);
                }
                return Ok(());
            }
            value += count;
            if fragment.has_zeroes {
                bit += count;
            }
        }
        Err("no fragment covers that property".into())
    }

    /// Gives a skipped slot a place in the header, so the stream carries a value for it, or with
    /// `is_zero` a zero flag. The fragment covering the slot is split around it and every other
    /// fragment keeps its bytes; the new fragment declares zeroes only when it must, so the mask
    /// bits of the others stay where they were.
    pub(crate) fn insert_value(&mut self, schema_index: u32, is_zero: bool) -> Result<(), String> {
        match self.place(schema_index) {
            Placement::Present { .. } => Err("this property is already in the header".into()),
            Placement::Skipped {
                fragment,
                offset,
                bit,
            } => {
                let old = self.fragments[fragment];
                let rest = Fragment {
                    skip: old.skip - offset - 1,
                    has_zeroes: old.has_zeroes,
                    value_num: old.value_num,
                    is_last: old.is_last,
                };
                let holds_nothing = rest.skip == 0 && rest.value_num == 0;
                self.fragments[fragment] = Fragment {
                    skip: offset,
                    has_zeroes: is_zero,
                    value_num: 1,
                    is_last: holds_nothing && rest.is_last,
                };
                if !holds_nothing {
                    self.fragments.insert(fragment + 1, rest);
                }
                if is_zero {
                    self.zero_bits.insert(bit, true);
                }
                self.insert_item(schema_index, is_zero);
                self.normalize();
                Ok(())
            }
            Placement::Beyond { covered, bit } => {
                let mut gap = schema_index - covered;
                if let Some(last) = self.fragments.last_mut() {
                    last.is_last = false;
                }
                while gap > FRAGMENT_CAP {
                    self.fragments.push(Fragment {
                        skip: FRAGMENT_CAP,
                        has_zeroes: false,
                        value_num: 0,
                        is_last: false,
                    });
                    gap -= FRAGMENT_CAP;
                }
                self.fragments.push(Fragment {
                    skip: gap,
                    has_zeroes: is_zero,
                    value_num: 1,
                    is_last: true,
                });
                if is_zero {
                    self.zero_bits.insert(bit, true);
                }
                self.insert_item(schema_index, is_zero);
                self.normalize();
                Ok(())
            }
        }
    }

    /// Takes a slot out of the header, so the loader leaves the property at the value it inherits.
    /// The fragment holding it is trimmed or split around it; every other fragment keeps its bytes.
    pub(crate) fn remove_value(&mut self, schema_index: u32) -> Result<(), String> {
        let Placement::Present {
            fragment,
            offset,
            bit,
        } = self.place(schema_index)
        else {
            return Err("this property is not in the header".into());
        };
        let old = self.fragments[fragment];
        if old.has_zeroes {
            self.zero_bits.remove(bit + offset as usize);
        }
        let after = old.value_num - offset - 1;
        if offset == 0 && old.skip < FRAGMENT_CAP {
            // The run starts one slot later.
            self.fragments[fragment].skip += 1;
            self.fragments[fragment].value_num -= 1;
        } else if after == 0 {
            // The run ends one value earlier; the slot becomes a skip the next fragment absorbs. Past
            // the last fragment nothing needs recording.
            self.fragments[fragment].value_num -= 1;
            match self.fragments.get_mut(fragment + 1) {
                Some(next) if next.skip < FRAGMENT_CAP => next.skip += 1,
                Some(_) => self.fragments.insert(
                    fragment + 1,
                    Fragment {
                        skip: 1,
                        has_zeroes: false,
                        value_num: 0,
                        is_last: false,
                    },
                ),
                None => {}
            }
        } else {
            // The run splits around the slot; the second half inherits the mask and the last flag.
            let tail = Fragment {
                skip: 1,
                has_zeroes: old.has_zeroes,
                value_num: after,
                is_last: old.is_last,
            };
            self.fragments[fragment] = Fragment {
                skip: old.skip,
                has_zeroes: old.has_zeroes,
                value_num: offset,
                is_last: false,
            };
            self.fragments.insert(fragment + 1, tail);
        }
        self.tidy(fragment);
        self.remove_item(schema_index);
        self.normalize();
        Ok(())
    }

    /// Writes the fragments the way UE's header builder does from the values they hold: slots taken
    /// in order, a run of skips or values closed only where it reaches 127 or the other kind
    /// follows, zeroes declared only by a fragment holding one, and the skips past the last value
    /// dropped. A header holding nothing is one fragment. A header UE wrote comes back unchanged.
    fn normalize(&mut self) {
        let (fragments, zero_bits) = if self.items.is_empty() {
            let skip = (self.covered_slots() as u32).min(FRAGMENT_CAP);
            (
                vec![Fragment {
                    skip,
                    has_zeroes: false,
                    value_num: 0,
                    is_last: true,
                }],
                Vec::new(),
            )
        } else {
            built(&self.items)
        };
        if zero_bits.len() != self.zero_bits.len() {
            // The bits moved, so the bytes they were read from no longer line up with them.
            self.zero_mask_bytes.clear();
        }
        self.fragments = fragments;
        self.zero_bits = zero_bits;
    }

    /// A fragment left with no values is folded into its neighbour where the skip fits, and skips
    /// trailing past the last value are dropped, so that inserting and then removing a slot gives
    /// back the header it started from.
    fn tidy(&mut self, fragment: usize) {
        if self.fragments[fragment].value_num != 0 {
            return;
        }
        self.fragments[fragment].has_zeroes = false;
        let skip = self.fragments[fragment].skip;
        match self.fragments.get_mut(fragment + 1) {
            Some(next) if skip + next.skip <= FRAGMENT_CAP => {
                next.skip += skip;
                self.fragments.remove(fragment);
            }
            Some(_) => {}
            None => {
                while self.fragments.len() > 1
                    && self
                        .fragments
                        .last()
                        .is_some_and(|last| last.value_num == 0)
                {
                    self.fragments.pop();
                }
                if let Some(last) = self.fragments.last_mut() {
                    last.is_last = true;
                }
            }
        }
    }

    fn insert_item(&mut self, schema_index: u32, is_zero: bool) {
        let at = self
            .items
            .partition_point(|item| item.schema_index < schema_index);
        self.items.insert(
            at,
            HeaderItem {
                schema_index,
                is_zero,
            },
        );
    }

    fn remove_item(&mut self, schema_index: u32) {
        if let Some(at) = self.ordinal(schema_index) {
            self.items.remove(at);
        }
    }
}

/// The header UE writes for a struct that stores none of its slots: one fragment skipping as many
/// of them as it can hold, flagged last, and no mask. This is how a struct is stored from nothing.
pub(crate) fn empty_header(slots: usize) -> Vec<u8> {
    let skip = slots.min(FRAGMENT_CAP as usize) as u16;
    (IS_LAST_MASK | skip).to_le_bytes().to_vec()
}

/// The header for a struct that stores every slot as zero, which is how a zero struct keeps being
/// zero once it is stored: a slot left out would take whatever the archetype holds instead.
pub(crate) fn zero_header(slots: usize) -> Result<Vec<u8>, String> {
    let empty = empty_header(slots);
    let mut header = read_header(&mut Cursor::new(&empty, 0))?;
    for slot in 0..slots {
        header.insert_value(slot as u32, true)?;
    }
    header.write()
}

/// UE packs the mask into the narrowest of a byte, a word, or a run of 32-bit words. Every one of
/// those is little-endian, so the mask is a flat bit array over its bytes whichever width it takes.
fn mask_len(bits: usize) -> usize {
    if bits == 0 {
        0
    } else if bits <= 8 {
        1
    } else if bits <= 16 {
        2
    } else {
        bits.div_ceil(32) * 4
    }
}

fn read_zero_mask(cursor: &mut Cursor<'_>, bits: u32) -> Result<Vec<bool>, String> {
    if bits == 0 {
        return Ok(Vec::new());
    }
    let mut mask = Vec::with_capacity(bits as usize);
    if bits <= 8 {
        let word = cursor.read_u8()?;
        for bit in 0..bits {
            mask.push(word >> bit & 1 == 1);
        }
    } else if bits <= 16 {
        let word = cursor.read_u16()?;
        for bit in 0..bits {
            mask.push(word >> bit & 1 == 1);
        }
    } else {
        for chunk in 0..bits.div_ceil(32) {
            let word = cursor.read_u32()?;
            let remaining = bits - chunk * 32;
            for bit in 0..remaining.min(32) {
                mask.push(word >> bit & 1 == 1);
            }
        }
    }
    Ok(mask)
}

/// Writes the flags back over the bytes they came from, so the undefined tail of the mask is kept
/// rather than zeroed. A mask that changed length has no original to preserve and is packed fresh.
fn write_zero_mask(out: &mut Vec<u8>, bits: &[bool], original: &[u8]) {
    let width = mask_len(bits.len());
    if width == 0 {
        return;
    }
    let mut bytes = if original.len() == width {
        original.to_vec()
    } else {
        vec![0u8; width]
    };
    for (index, set) in bits.iter().enumerate() {
        let mask = 1u8 << (index % 8);
        if *set {
            bytes[index / 8] |= mask;
        } else {
            bytes[index / 8] &= !mask;
        }
    }
    out.extend_from_slice(&bytes);
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn packed(skip: u16, has_zeroes: bool, value_num: u16, is_last: bool) -> u16 {
        skip | (u16::from(has_zeroes) << 7)
            | (u16::from(is_last) << 8)
            | (value_num << VALUE_NUM_SHIFT)
    }

    fn read(data: &[u8]) -> UnversionedHeader {
        let mut cursor = Cursor::new(data, 0);
        read_header(&mut cursor).expect("header")
    }

    /// A slot put in moves every slot at or above it up, the values keeping their order and
    /// whether each is zero, and the header written for them reads back the same.
    #[test]
    fn shifting_slots_keeps_the_values_and_their_zero_flags() {
        let mut header = read(&empty_header(10));
        header.insert_value(0, false).expect("stored");
        header.insert_value(2, true).expect("stored");
        header.insert_value(5, false).expect("stored");
        header.shift_from(2, 1);
        assert_eq!(indices(&header), [0, 3, 6]);
        let zeros: Vec<bool> = header.items.iter().map(|item| item.is_zero).collect();
        assert_eq!(zeros, [false, true, false]);
        let again = read(&header.write().expect("written"));
        assert_eq!(indices(&again), [0, 3, 6]);
        assert!(again.items[1].is_zero);
        header.shift_from(0, 2);
        assert_eq!(indices(&header), [2, 5, 8]);
    }

    /// A stored zero struct holds every slot, each flagged zero, so none takes the archetype's
    /// value; past one fragment's worth the slots run on into another.
    #[test]
    fn a_zero_header_stores_every_slot_as_zero() {
        for slots in [0, 3, 200] {
            let header = read(&zero_header(slots).expect("zero header"));
            assert_eq!(indices(&header), (0..slots as u32).collect::<Vec<_>>());
            assert!(
                header.items.iter().all(|item| item.is_zero),
                "{slots} slots"
            );
        }
    }

    fn written(header: &UnversionedHeader) -> Vec<u8> {
        header.write().expect("write")
    }

    fn indices(header: &UnversionedHeader) -> Vec<u32> {
        header.items.iter().map(|i| i.schema_index).collect()
    }

    fn zeroes(header: &UnversionedHeader) -> Vec<bool> {
        header.items.iter().map(|i| i.is_zero).collect()
    }

    #[test]
    fn a_packed_fragment_round_trips_through_unpack() {
        let fragment = unpack(packed(5, true, 9, true));
        assert_eq!(fragment.skip, 5);
        assert!(fragment.has_zeroes);
        assert_eq!(fragment.value_num, 9);
        assert!(fragment.is_last);
    }

    #[test]
    fn skips_advance_the_schema_cursor_without_emitting_items() {
        let header = read(&packed(3, false, 2, true).to_le_bytes());
        assert_eq!(indices(&header), [3, 4]);
        assert!(header.items.iter().all(|i| !i.is_zero));
    }

    #[test]
    fn several_fragments_accumulate_their_skips() {
        let mut data = packed(1, false, 1, false).to_le_bytes().to_vec();
        data.extend_from_slice(&packed(2, false, 1, true).to_le_bytes());
        let header = read(&data);
        assert_eq!(indices(&header), [1, 4]);
    }

    #[test]
    fn a_byte_wide_zero_mask_marks_only_the_flagged_values() {
        let mut data = packed(0, true, 3, true).to_le_bytes().to_vec();
        data.push(0b0000_0101);
        let header = read(&data);
        assert_eq!(zeroes(&header), [true, false, true]);
    }

    #[test]
    fn a_seventeen_bit_zero_mask_is_read_as_thirty_two_bit_words() {
        let mut data = packed(0, true, 17, true).to_le_bytes().to_vec();
        data.extend_from_slice(&(1u32 << 16).to_le_bytes());
        let header = read(&data);
        assert_eq!(header.items.len(), 17);
        assert!(header.items[16].is_zero);
        assert!(!header.items[0].is_zero);
    }

    #[test]
    fn a_sixteen_bit_zero_mask_uses_a_single_word_not_a_dword() {
        let mut data = packed(0, true, 16, true).to_le_bytes().to_vec();
        data.extend_from_slice(&0x8000u16.to_le_bytes());
        let header = read(&data);
        assert_eq!(header.items.len(), 16);
        assert!(header.items[15].is_zero);
    }

    #[test]
    fn zero_mask_bits_are_consumed_only_by_fragments_that_declare_them() {
        let mut data = packed(0, false, 2, false).to_le_bytes().to_vec();
        data.extend_from_slice(&packed(0, true, 2, true).to_le_bytes());
        data.push(0b0000_0010);
        let header = read(&data);
        assert_eq!(zeroes(&header), [false, false, false, true]);
    }

    /// The mask bits past the last value are undefined, and a header written by something other
    /// than the game may have set them. Re-packing from the flags alone would quietly rewrite
    /// bytes no edit touched.
    #[test]
    fn the_undefined_tail_of_a_mask_survives_re_emission() {
        let mut data = packed(0, true, 3, true).to_le_bytes().to_vec();
        data.push(0b1010_1101);
        let header = read(&data);
        assert_eq!(zeroes(&header), [true, false, true]);
        assert_eq!(
            written(&header),
            data,
            "the high bits are not ours to clear"
        );
    }

    /// Changing a flag has to change that bit and leave the rest of the byte as it was.
    #[test]
    fn storing_a_value_touches_one_bit_of_a_mask_with_a_dirty_tail() {
        let mut data = packed(0, true, 3, true).to_le_bytes().to_vec();
        data.push(0b1111_0101);
        let mut header = read(&data);
        header.store(0).expect("store");
        assert_eq!(written(&header)[2], 0b1111_0100);
    }

    /// A header written by something other than the game may carry empty fragments and split
    /// runs. Rebuilding it from the items would drop them and rewrite bytes no edit touched.
    #[test]
    fn a_header_carrying_empty_fragments_is_re_emitted_exactly() {
        let mut data = packed(0, false, 0, false).to_le_bytes().to_vec();
        data.extend_from_slice(&packed(0, false, 0, false).to_le_bytes());
        data.extend_from_slice(&packed(2, false, 8, false).to_le_bytes());
        data.extend_from_slice(&packed(0, false, 0, false).to_le_bytes());
        data.extend_from_slice(&packed(0, false, 40, true).to_le_bytes());
        let header = read(&data);
        assert_eq!(header.items.len(), 48);
        assert_eq!(written(&header), data);
    }

    /// The audit counts these shapes over the game's packages to learn which ones UE never writes.
    #[test]
    fn a_header_names_the_shapes_it_takes() {
        let mut data = packed(0, false, 0, false).to_le_bytes().to_vec();
        data.extend_from_slice(&packed(2, true, 2, false).to_le_bytes());
        data.extend_from_slice(&packed(1, true, 1, true).to_le_bytes());
        data.push(0b1111_0100);
        let shapes = read(&data).shapes();
        assert!(shapes.contains(&SHAPE_EMPTY_FRAGMENT), "{shapes:?}");
        assert!(shapes.contains(&SHAPE_ZERO_FLAG_WITHOUT_ZERO), "{shapes:?}");
        assert!(shapes.contains(&SHAPE_DIRTY_MASK_TAIL), "{shapes:?}");
        assert!(!shapes.contains(&SHAPE_ENDS_ON_SKIP), "{shapes:?}");

        let mut spread = packed(127, false, 0, false).to_le_bytes().to_vec();
        spread.extend_from_slice(&packed(40, false, 0, true).to_le_bytes());
        assert!(read(&spread).shapes().contains(&SHAPE_EMPTY_OVER_FRAGMENTS));
        assert!(read(&empty_header(300)).shapes().is_empty());
        assert!(read(&empty_header(3)).shapes().is_empty());

        let mut split = packed(1, false, 2, false).to_le_bytes().to_vec();
        split.extend_from_slice(&packed(0, false, 1, true).to_le_bytes());
        let shapes = read(&split).shapes();
        assert!(shapes.contains(&SHAPE_SPLIT_RUN), "{shapes:?}");
        assert!(shapes.contains(&SHAPE_NOT_BUILT), "{shapes:?}");
    }

    /// An edit leaves the header UE's builder would write for the values it ends up holding: a slot
    /// stored beside a run joins it rather than splitting it, and storing a fragment's last zero
    /// drops its mask.
    #[test]
    fn an_edited_header_is_the_one_ue_builds() {
        let mut data = packed(0, false, 2, false).to_le_bytes().to_vec();
        data.extend_from_slice(&packed(1, true, 2, true).to_le_bytes());
        data.push(0b0000_0010);
        let mut header = read(&data);
        header.insert_value(2, false).expect("insert");
        assert_eq!(
            written(&header),
            [
                packed(0, true, 5, true).to_le_bytes().to_vec(),
                vec![0b0001_0000]
            ]
            .concat()
        );
        header.store(4).expect("store");
        assert_eq!(written(&header), packed(0, false, 5, true).to_le_bytes());
        assert!(header.shapes().is_empty(), "{:?}", header.shapes());

        let mut long = read(&empty_header(0));
        for slot in 0..130 {
            long.insert_value(slot, true).expect("insert");
        }
        let expected = [
            packed(0, true, 127, false).to_le_bytes().to_vec(),
            packed(0, true, 3, true).to_le_bytes().to_vec(),
            vec![0xFF; 16],
            vec![0b0000_0011, 0, 0, 0],
        ]
        .concat();
        assert_eq!(written(&long), expected);
        assert_eq!(zero_header(130).expect("zero header"), expected);
    }

    #[test]
    fn a_trailing_skip_past_the_last_value_survives_re_emission() {
        let data = packed(3, false, 0, true).to_le_bytes();
        let header = read(&data);
        assert!(header.items.is_empty());
        assert_eq!(written(&header), data);
    }

    #[test]
    fn storing_a_defaulted_value_clears_its_bit_and_keeps_the_header_length() {
        let mut data = packed(0, true, 3, true).to_le_bytes().to_vec();
        data.push(0b0000_0101);
        let mut header = read(&data);
        header.store(2).expect("store");
        let out = written(&header);
        assert_eq!(out.len(), data.len());
        assert_eq!(out[2], 0b0000_0001);
        assert!(!read(&out).items[2].is_zero);
    }

    #[test]
    fn clearing_a_value_in_a_fragment_without_a_mask_grows_the_header_by_the_mask() {
        let data = packed(0, false, 3, true).to_le_bytes();
        let mut header = read(&data);
        header.clear(1).expect("clear");
        let out = written(&header);
        assert_eq!(out.len(), data.len() + 1, "the mask byte is new");
        assert_eq!(zeroes(&read(&out)), [false, true, false]);
    }

    #[test]
    fn clearing_a_value_leaves_the_mask_bits_of_earlier_fragments_where_they_were() {
        let mut data = packed(0, true, 2, false).to_le_bytes().to_vec();
        data.extend_from_slice(&packed(0, false, 2, true).to_le_bytes());
        data.push(0b0000_0010);
        let mut header = read(&data);
        header.clear(3).expect("clear");
        assert_eq!(zeroes(&read(&written(&header))), [false, true, false, true]);
    }

    /// Slots are addressed by schema index, not by position among the items, because the item a
    /// skipped slot would become does not exist yet.
    #[test]
    fn slots_are_addressed_by_schema_index() {
        let mut data = packed(5, true, 2, true).to_le_bytes().to_vec();
        data.push(0b0000_0001);
        let mut header = read(&data);
        assert!(header.store(0).is_err(), "slot 0 is skipped, not an item");
        header.store(5).expect("slot 5 is the first item");
        assert_eq!(zeroes(&read(&written(&header))), [false, false]);
    }

    /// A slot at the very start of a skip run.
    #[test]
    fn inserting_at_the_start_of_a_skip_run_puts_the_value_first() {
        let data = packed(3, false, 2, true).to_le_bytes();
        let mut header = read(&data);
        header.insert_value(0, false).expect("insert");
        let out = written(&header);
        assert_eq!(out.len(), 4, "one more fragment, no mask");
        let again = read(&out);
        assert_eq!(indices(&again), [0, 3, 4]);
        assert_eq!(zeroes(&again), [false; 3]);
    }

    #[test]
    fn inserting_in_the_middle_of_a_skip_run_splits_it() {
        let data = packed(3, false, 2, true).to_le_bytes();
        let mut header = read(&data);
        header.insert_value(1, false).expect("insert");
        assert_eq!(indices(&read(&written(&header))), [1, 3, 4]);
    }

    /// The slot just before the run's values leaves the follower with no skip at all.
    #[test]
    fn inserting_at_the_end_of_a_skip_run_leaves_the_values_adjacent() {
        let data = packed(3, false, 2, true).to_le_bytes();
        let mut header = read(&data);
        header.insert_value(2, false).expect("insert");
        let again = read(&written(&header));
        assert_eq!(indices(&again), [2, 3, 4]);
    }

    /// The new fragment declares no zeroes, so the bits of the run it split are untouched.
    #[test]
    fn inserting_into_a_run_with_zeroes_leaves_its_mask_where_it_was() {
        let mut data = packed(2, true, 3, true).to_le_bytes().to_vec();
        data.push(0b0000_0101);
        let mut header = read(&data);
        header.insert_value(0, false).expect("insert");
        let again = read(&written(&header));
        assert_eq!(indices(&again), [0, 2, 3, 4]);
        assert_eq!(zeroes(&again), [false, true, false, true]);
    }

    /// A zero flag for a skipped slot needs a bit of its own, spliced in before the bits of the run
    /// that follows it.
    #[test]
    fn inserting_a_zero_adds_one_bit_in_front_of_the_later_ones() {
        let mut data = packed(2, true, 3, true).to_le_bytes().to_vec();
        data.push(0b0000_0101);
        let mut header = read(&data);
        header.insert_value(0, true).expect("insert");
        let again = read(&written(&header));
        assert_eq!(indices(&again), [0, 2, 3, 4]);
        assert_eq!(zeroes(&again), [true, true, false, true]);
    }

    /// Fragments count in seven bits, so a slot far past the header's reach takes several.
    #[test]
    fn inserting_beyond_the_header_appends_skip_fragments_in_chunks_of_127() {
        let data = packed(0, false, 2, true).to_le_bytes();
        let mut header = read(&data);
        header.insert_value(300, false).expect("insert");
        let out = written(&header);
        assert_eq!(out.len(), 8, "the old fragment and three new ones");
        let again = read(&out);
        assert_eq!(indices(&again), [0, 1, 300]);
    }

    /// A header that is nothing but a trailing skip gains its first value and stays one fragment.
    #[test]
    fn inserting_into_a_pure_skip_replaces_it() {
        let data = packed(1, false, 0, true).to_le_bytes();
        let mut header = read(&data);
        header.insert_value(0, false).expect("insert");
        let out = written(&header);
        assert_eq!(out.len(), 2);
        assert_eq!(indices(&read(&out)), [0]);
    }

    #[test]
    fn removing_the_first_value_of_a_run_lengthens_its_skip() {
        let data = packed(1, false, 4, true).to_le_bytes();
        let mut header = read(&data);
        header.remove_value(1).expect("remove");
        let out = written(&header);
        assert_eq!(out, packed(2, false, 3, true).to_le_bytes());
    }

    #[test]
    fn removing_a_middle_value_splits_the_run_and_its_mask() {
        let mut data = packed(1, true, 4, true).to_le_bytes().to_vec();
        data.push(0b0000_1010);
        let mut header = read(&data);
        header.remove_value(3).expect("remove");
        let again = read(&written(&header));
        assert_eq!(indices(&again), [1, 2, 4]);
        assert_eq!(zeroes(&again), [false, true, true]);
    }

    #[test]
    fn removing_the_last_value_of_the_last_run_needs_no_trailing_skip() {
        let data = packed(1, false, 4, true).to_le_bytes();
        let mut header = read(&data);
        header.remove_value(4).expect("remove");
        assert_eq!(written(&header), packed(1, false, 3, true).to_le_bytes());
    }

    #[test]
    fn removing_the_last_value_of_a_run_hands_the_slot_to_the_next_fragment() {
        let mut data = packed(0, false, 2, false).to_le_bytes().to_vec();
        data.extend_from_slice(&packed(1, false, 1, true).to_le_bytes());
        let mut header = read(&data);
        header.remove_value(1).expect("remove");
        let mut want = packed(0, false, 1, false).to_le_bytes().to_vec();
        want.extend_from_slice(&packed(2, false, 1, true).to_le_bytes());
        assert_eq!(written(&header), want);
    }

    /// Inserting a slot and taking it out again must give back the header it started from, or two
    /// saves would leave a package drifting.
    #[test]
    fn inserting_then_removing_restores_the_original_bytes() {
        let mut data = packed(3, true, 2, false).to_le_bytes().to_vec();
        data.extend_from_slice(&packed(2, false, 1, true).to_le_bytes());
        data.push(0b0000_0010);
        for slot in [0u32, 1, 2, 6, 40, 300] {
            let mut header = read(&data);
            header.insert_value(slot, false).expect("insert");
            header.remove_value(slot).expect("remove");
            assert_eq!(written(&header), data, "slot {slot}");
        }
    }

    /// Several edits land on one cached header within a save, so the fragment bookkeeping has to
    /// stay right across them.
    #[test]
    fn inserting_then_clearing_another_slot_keeps_the_items_consistent() {
        let data = packed(0, false, 3, true).to_le_bytes();
        let mut header = read(&data);
        header.insert_value(5, false).expect("insert");
        header.clear(1).expect("clear");
        let again = read(&written(&header));
        assert_eq!(indices(&again), [0, 1, 2, 5]);
        assert_eq!(zeroes(&again), [false, true, false, false]);
        assert_eq!(indices(&header), indices(&again));
        assert_eq!(zeroes(&header), zeroes(&again));
    }

    /// A struct stored from nothing starts as a skip over every slot. Values stored into it end the
    /// header on the last of them, as UE writes one: its loader steps past a fragment with no values
    /// to reach the next, so a header ending on one sends it past the end of the fragments.
    #[test]
    fn values_stored_into_an_empty_struct_end_the_header() {
        let empty = read(&empty_header(40));
        assert!(!empty.ends_on_skip(), "a header holding nothing is a skip");

        let mut ending_on_a_skip = packed(1, false, 1, false).to_le_bytes().to_vec();
        ending_on_a_skip.extend_from_slice(&packed(28, false, 0, true).to_le_bytes());
        let err = read_header(&mut Cursor::new(&ending_on_a_skip, 0)).expect_err("refused");
        assert!(err.contains("reads past"), "{err}");

        let bytes = |fragments: &[u16]| -> Vec<u8> {
            fragments.iter().flat_map(|f| f.to_le_bytes()).collect()
        };
        let mut header = read(&empty_header(59));
        header.insert_value(1, false).expect("insert");
        header.insert_value(30, false).expect("insert");
        assert_eq!(
            written(&header),
            bytes(&[packed(1, false, 1, false), packed(28, false, 1, true)])
        );

        let mut header = read(&empty_header(300));
        header.insert_value(5, false).expect("insert");
        assert_eq!(written(&header), bytes(&[packed(5, false, 1, true)]));
        header.insert_value(200, true).expect("insert");
        let again = read(&written(&header));
        assert_eq!(indices(&again), [5, 200]);
        assert_eq!(zeroes(&again), [false, true]);
        assert!(
            again
                .fragments
                .last()
                .is_some_and(|last| last.value_num > 0)
        );
    }

    /// The class export of `CameraShake_101111` stores none of UClass's fourteen slots and reads
    /// `0E 01`; that is the form to reproduce.
    #[test]
    fn an_empty_header_is_the_skip_count_ue_writes() {
        assert_eq!(empty_header(14), [0x0E, 0x01]);
        assert_eq!(empty_header(0), [0x00, 0x01]);
        assert_eq!(empty_header(300), [0x7F, 0x01]);
        let header = read(&empty_header(300));
        assert!(header.items.is_empty());
        assert!(matches!(header.place(126), Placement::Skipped { .. }));
        assert!(matches!(header.place(127), Placement::Beyond { .. }));
    }

    #[test]
    fn a_fragment_over_the_seven_bit_caps_is_refused_rather_than_truncated() {
        let fragment = Fragment {
            skip: 128,
            has_zeroes: false,
            value_num: 0,
            is_last: true,
        };
        assert!(pack(&fragment).is_err());
        let fragment = Fragment {
            skip: 0,
            has_zeroes: false,
            value_num: 128,
            is_last: true,
        };
        assert!(pack(&fragment).is_err());
    }
}
