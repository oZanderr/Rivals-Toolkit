//! Loads a .usmap file and flattens each struct's inherited schema into declaration-order slots.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use usmap::{Property, PropertyInner, Struct, Usmap};

/// Guards against a cyclic super-struct chain in a malformed mappings file.
const MAX_INHERITANCE_DEPTH: u32 = 128;

/// One schema slot. A property declared as `float Foo[4]` occupies four consecutive slots, so the
/// element index has to travel with the property.
#[derive(Clone, Copy)]
struct Slot {
    struct_index: u32,
    property_index: u32,
    element: u32,
}

pub struct Mappings {
    inner: Arc<Usmap>,
    structs: HashMap<String, u32>,
    /// Every entry of a name the file holds more than one of, in file order: one per Blueprint
    /// for a generated type, one per module for a script class. The default choice is the
    /// fullest; a package that reads wrongly under it can be tried against the others.
    twins: HashMap<String, Vec<u32>>,
    enums: HashMap<String, u32>,
    flat: Vec<Vec<Slot>>,
}

impl Mappings {
    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        check_supported_compression(bytes)?;
        let inner = Usmap::read(&mut std::io::Cursor::new(bytes))
            .map_err(|e| format!("parse usmap: {e:#}"))?;
        Ok(Self::from_usmap(inner))
    }

    /// A schema source built from structs recovered out of the game's own packages rather than a
    /// mappings file. See [`crate::ustruct`].
    pub(crate) fn from_structs(structs: Vec<usmap::Struct>) -> Self {
        Self::from_structs_and_enums(structs, Vec::new())
    }

    pub(crate) fn from_structs_and_enums(
        structs: Vec<usmap::Struct>,
        enums: Vec<usmap::Enum>,
    ) -> Self {
        Self::from_usmap(Usmap {
            enums,
            structs,
            cext: None,
            ppth: None,
            eatr: None,
            envp: None,
        })
    }

    fn from_usmap(inner: Usmap) -> Self {
        // Names repeat: a dump carries one entry per Blueprint for generated types, and stubs whose
        // super is their own name. Collecting into a map would keep whichever came last, which is
        // usually an empty stub, so pick the fullest entry that at least describes itself.
        let mut structs: HashMap<String, u32> = HashMap::new();
        let mut twins: HashMap<String, Vec<u32>> = HashMap::new();
        for (index, entry) in inner.structs.iter().enumerate() {
            if entry.super_struct.as_deref() == Some(entry.name.as_str()) {
                continue;
            }
            twins
                .entry(entry.name.clone())
                .or_default()
                .push(index as u32);
            let better = match structs.get(&entry.name) {
                Some(existing) => inner
                    .structs
                    .get(*existing as usize)
                    .is_none_or(|s| s.properties.len() < entry.properties.len()),
                None => true,
            };
            if better {
                structs.insert(entry.name.clone(), index as u32);
            }
        }
        twins.retain(|_, entries| entries.len() > 1);
        let enums: HashMap<String, u32> = inner
            .enums
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i as u32))
            .collect();
        let flat = flatten_all(&inner, &structs);
        Self {
            inner: Arc::new(inner),
            structs,
            twins,
            enums,
            flat,
        }
    }

    /// The entries `name` has when the file holds more than one, in file order; empty otherwise.
    pub fn twins(&self, name: &str) -> &[u32] {
        self.twins.get(name).map_or(&[], Vec::as_slice)
    }

    /// The entry `name` resolves to.
    pub fn chosen(&self, name: &str) -> Option<u32> {
        self.structs.get(name).copied()
    }

    /// These mappings with `name` resolving to entry `index` instead, every schema flattened
    /// again so a chain through the name follows that twin.
    pub fn with_twin(&self, name: &str, index: u32) -> Mappings {
        let mut structs = self.structs.clone();
        structs.insert(name.to_string(), index);
        let flat = flatten_all(&self.inner, &structs);
        Mappings {
            inner: Arc::clone(&self.inner),
            structs,
            twins: self.twins.clone(),
            enums: self.enums.clone(),
            flat,
        }
    }

    /// The entries these mappings hold that `onto` holds under no twin of the same name: the
    /// definitions recovered from packages, as opposed to the parent chains copied from a file.
    pub fn recovered(&self, onto: &Mappings) -> Vec<Struct> {
        self.inner
            .structs
            .iter()
            .filter(|entry| !onto.entries_named(&entry.name).any(|held| held == *entry))
            .cloned()
            .collect()
    }

    fn entries_named<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Struct> + 'a {
        let mut indices: Vec<u32> = self.twins(name).to_vec();
        indices.extend(self.chosen(name));
        indices
            .into_iter()
            .filter_map(move |index| self.inner.structs.get(index as usize))
    }

    pub fn struct_count(&self) -> usize {
        self.inner.structs.len()
    }

    pub fn enum_count(&self) -> usize {
        self.inner.enums.len()
    }

    pub fn has_struct(&self, name: &str) -> bool {
        self.structs.contains_key(name)
    }

    pub fn schema(&self, name: &str) -> Option<Schema<'_>> {
        self.schema_fixed(name, None)
    }

    /// The same schema with the slots in `fixups` elided. See [`SchemaFixups`].
    pub fn schema_fixed<'a>(
        &'a self,
        name: &str,
        fixups: Option<&'a SchemaFixups>,
    ) -> Option<Schema<'a>> {
        let index = *self.structs.get(name)?;
        Some(Schema {
            mappings: self,
            name: &self.inner.structs.get(index as usize)?.name,
            slots: self.flat.get(index as usize)?,
            skip: fixups.map_or(&[][..], |f| f.get(name)),
        })
    }

    /// The schema for an export's class.
    ///
    /// The mappings hold classes and structs in one table keyed by the name with its prefix
    /// stripped, so a class the file omits collides with a same-named struct: `UDamageEvent` and
    /// `FDamageEvent` are both `DamageEvent`. Every class descends from `Object` and no struct
    /// does, so a chain that roots anywhere else belongs to a struct, and decoding an export
    /// against it produces confident nonsense.
    pub fn class_schema<'a>(
        &'a self,
        name: &str,
        fixups: Option<&'a SchemaFixups>,
    ) -> Option<Schema<'a>> {
        if self.ancestry(name).first() != Some(&"Object") {
            return None;
        }
        self.schema_fixed(name, fixups)
    }

    /// Every struct type reachable from `name`'s properties, including through containers. The
    /// `diagnose` sweep needs these because a nested struct that reads the wrong width desyncs its
    /// parent without reporting the error against itself.
    pub fn struct_references(&self, name: &str, depth: u32) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut frontier = vec![name.to_string()];
        for _ in 0..depth {
            let mut next = Vec::new();
            for current in frontier {
                let Some(schema) = self.schema(&current) else {
                    continue;
                };
                for slot in schema.iter() {
                    collect_struct_names(&slot.property.inner, &mut next);
                }
            }
            frontier = next
                .into_iter()
                .filter(|n| seen.insert(n.clone()))
                .collect();
            if frontier.is_empty() {
                break;
            }
        }
        seen.into_iter().collect()
    }

    /// Whether `name` is `ancestor` or derives from it. Games subclass engine types such as
    /// UDataTable, so matching on the exact class name misses them.
    pub fn inherits_from(&self, name: &str, ancestor: &str) -> bool {
        let mut current = Some(name.to_string());
        for _ in 0..MAX_INHERITANCE_DEPTH {
            let Some(step) = current else {
                return false;
            };
            if step == ancestor {
                return true;
            }
            current = self
                .structs
                .get(&step)
                .and_then(|i| self.inner.structs.get(*i as usize))
                .and_then(|s| s.super_struct.clone());
        }
        false
    }

    /// The class chain from the most distant ancestor down to `name`, which is the order
    /// `Super::Serialize` runs in and therefore the order class tails appear on disk.
    pub fn ancestry(&self, name: &str) -> Vec<&str> {
        let mut chain = Vec::new();
        let mut current = Some(name.to_string());
        for _ in 0..MAX_INHERITANCE_DEPTH {
            let Some(step) = current else { break };
            let Some(&index) = self.structs.get(&step) else {
                break;
            };
            let Some(entry) = self.inner.structs.get(index as usize) else {
                break;
            };
            chain.push(entry.name.as_str());
            current = entry.super_struct.clone();
        }
        chain.reverse();
        chain
    }

    /// The first parent named up `name`'s chain that these mappings do not define, or `None` when
    /// the chain roots properly or `name` itself is unknown.
    pub fn missing_ancestor(&self, name: &str) -> Option<String> {
        let mut current = name.to_string();
        for _ in 0..MAX_INHERITANCE_DEPTH {
            let Some(&index) = self.structs.get(&current) else {
                return (current != name).then_some(current);
            };
            let entry = self.inner.structs.get(index as usize)?;
            current = entry.super_struct.clone()?;
        }
        None
    }

    /// Copies of the entries from `name` up to its root, for a mappings that has to carry a
    /// class's whole chain on its own.
    pub fn chain_entries(&self, name: &str) -> Vec<usmap::Struct> {
        self.ancestry(name)
            .into_iter()
            .filter_map(|step| {
                let index = *self.structs.get(step)?;
                self.inner.structs.get(index as usize).cloned()
            })
            .collect()
    }

    /// Census of every property type declared anywhere in the mappings, container inners
    /// included. This is what separates a code path that is merely unexercised from one that is
    /// unreachable: a type absent here can never appear in an asset for this game.
    pub fn property_kind_counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for entry in &self.inner.structs {
            for property in &entry.properties {
                count_kinds(&property.inner, &mut counts);
            }
        }
        counts
    }

    /// The value behind an enumerator name, for the container elements UE writes by name.
    pub fn enum_value(&self, enum_name: &str, entry: &str) -> Option<i64> {
        let index = *self.enums.get(enum_name)?;
        self.inner
            .enums
            .get(index as usize)?
            .entries
            .iter()
            .find(|(_, name)| name.as_str() == entry)
            .map(|(value, _)| *value)
    }

    /// Every enumerator of an enum in value order, empty for one the mappings do not know.
    pub fn enumerators(&self, enum_name: &str) -> Vec<(i64, String)> {
        self.enums
            .get(enum_name)
            .and_then(|&index| self.inner.enums.get(index as usize))
            .map(|entry| {
                entry
                    .entries
                    .iter()
                    .map(|(value, name)| (*value, name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolves an enum entry, returning `None` for values the mappings do not cover.
    pub fn enum_name(&self, enum_name: &str, value: i64) -> Option<&str> {
        let index = *self.enums.get(enum_name)?;
        self.inner
            .enums
            .get(index as usize)?
            .entries
            .get(&value)
            .map(String::as_str)
    }
}

fn count_kinds(inner: &PropertyInner, counts: &mut BTreeMap<&'static str, usize>) {
    *counts.entry(kind_name(inner)).or_default() += 1;
    match inner {
        PropertyInner::Array { inner } | PropertyInner::Optional { inner } => {
            count_kinds(inner, counts);
        }
        PropertyInner::Enum { inner, .. } => count_kinds(inner, counts),
        PropertyInner::Set { key } => count_kinds(key, counts),
        PropertyInner::Map { key, value } => {
            count_kinds(key, counts);
            count_kinds(value, counts);
        }
        _ => {}
    }
}

/// Stable name for a property type, shared by the static census and the runtime histogram so the
/// two can be compared directly.
pub fn kind_name(inner: &PropertyInner) -> &'static str {
    match inner {
        PropertyInner::Byte => "Byte",
        PropertyInner::Bool => "Bool",
        PropertyInner::Int => "Int",
        PropertyInner::Float => "Float",
        PropertyInner::Object => "Object",
        PropertyInner::Name => "Name",
        PropertyInner::Delegate => "Delegate",
        PropertyInner::Double => "Double",
        PropertyInner::Array { .. } => "Array",
        PropertyInner::Struct { .. } => "Struct",
        PropertyInner::Str => "Str",
        PropertyInner::Text => "Text",
        PropertyInner::Interface => "Interface",
        PropertyInner::MulticastDelegate => "MulticastDelegate",
        PropertyInner::WeakObject => "WeakObject",
        PropertyInner::LazyObject => "LazyObject",
        PropertyInner::AssetObject => "AssetObject",
        PropertyInner::SoftObject => "SoftObject",
        PropertyInner::UInt64 => "UInt64",
        PropertyInner::UInt32 => "UInt32",
        PropertyInner::UInt16 => "UInt16",
        PropertyInner::Int64 => "Int64",
        PropertyInner::Int16 => "Int16",
        PropertyInner::Int8 => "Int8",
        PropertyInner::Map { .. } => "Map",
        PropertyInner::Set { .. } => "Set",
        PropertyInner::Enum { .. } => "Enum",
        PropertyInner::FieldPath => "FieldPath",
        PropertyInner::Optional { .. } => "Optional",
        PropertyInner::Utf8Str => "Utf8Str",
        PropertyInner::AnsiStr => "AnsiStr",
        PropertyInner::Unknown => "Unknown",
    }
}

/// Flattened slots a struct's schema declares that the build being read does not serialize.
///
/// The mappings file describes the classes a build reflects, which is not always the set of
/// properties its cooked packages carry. Where the two disagree every property after the extra one
/// decodes into the wrong field, so the reader searches for the disagreement and records it here.
/// Nothing in this type is hard-coded: entries are derived from the bytes.
#[derive(Default, Debug, Clone)]
pub struct SchemaFixups {
    by_struct: BTreeMap<String, Vec<usize>>,
}

impl SchemaFixups {
    pub fn is_empty(&self) -> bool {
        self.by_struct.is_empty()
    }

    pub fn one(name: &str, slot: usize) -> Self {
        let mut fixups = Self::default();
        fixups.add(name, slot);
        fixups
    }

    pub fn add(&mut self, name: &str, slot: usize) {
        let slots = self.by_struct.entry(name.to_string()).or_default();
        if let Err(at) = slots.binary_search(&slot) {
            slots.insert(at, slot);
        }
    }

    pub fn get(&self, name: &str) -> &[usize] {
        self.by_struct.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[usize])> {
        self.by_struct
            .iter()
            .map(|(name, slots)| (name.as_str(), slots.as_slice()))
    }
}

pub struct Schema<'a> {
    mappings: &'a Mappings,
    name: &'a str,
    slots: &'a [Slot],
    skip: &'a [usize],
}

pub struct SchemaSlot<'a> {
    pub property: &'a Property,
    pub element: u32,
    pub owner: &'a str,
}

impl<'a> Schema<'a> {
    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn len(&self) -> usize {
        let elided = self.skip.iter().filter(|s| **s < self.slots.len()).count();
        self.slots.len() - elided
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn slot(&self, index: usize) -> Option<SchemaSlot<'a>> {
        let mut index = index;
        for elided in self.skip {
            if index >= *elided {
                index += 1;
            }
        }
        let slot = self.slots.get(index)?;
        let owner = self
            .mappings
            .inner
            .structs
            .get(slot.struct_index as usize)?;
        Some(SchemaSlot {
            property: owner.properties.get(slot.property_index as usize)?,
            element: slot.element,
            owner: &owner.name,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = SchemaSlot<'a>> + '_ {
        (0..self.len()).filter_map(|i| self.slot(i))
    }
}

fn collect_struct_names(inner: &PropertyInner, out: &mut Vec<String>) {
    match inner {
        PropertyInner::Struct { name } => out.push(name.clone()),
        PropertyInner::Array { inner } | PropertyInner::Optional { inner } => {
            collect_struct_names(inner, out)
        }
        PropertyInner::Set { key } => collect_struct_names(key, out),
        PropertyInner::Map { key, value } => {
            collect_struct_names(key, out);
            collect_struct_names(value, out);
        }
        _ => {}
    }
}

fn flatten_all(inner: &Usmap, structs: &HashMap<String, u32>) -> Vec<Vec<Slot>> {
    let mut done: Vec<Option<Vec<Slot>>> = vec![None; inner.structs.len()];
    for index in 0..inner.structs.len() {
        flatten_one(inner, structs, index as u32, &mut done, 0);
    }
    done.into_iter().map(Option::unwrap_or_default).collect()
}

fn flatten_one(
    inner: &Usmap,
    structs: &HashMap<String, u32>,
    index: u32,
    done: &mut Vec<Option<Vec<Slot>>>,
    depth: u32,
) {
    if depth > MAX_INHERITANCE_DEPTH {
        return;
    }
    let Some(entry) = inner.structs.get(index as usize) else {
        return;
    };
    if done.get(index as usize).is_some_and(Option::is_some) {
        return;
    }

    // A struct's own properties come first and the inherited ones follow, matching the order
    // UE builds `PropertyLink` in. Every property index in a usmap is local to its own struct,
    // so a class with a deep chain decodes into the wrong fields if this is reversed.
    let mut slots = Vec::new();
    let mut ordered: Vec<u32> = (0..entry.properties.len() as u32).collect();
    ordered.sort_by_key(|i| {
        entry
            .properties
            .get(*i as usize)
            .map_or(u16::MAX, |p| p.index)
    });

    for property_index in ordered {
        let Some(property) = entry.properties.get(property_index as usize) else {
            continue;
        };
        for element in 0..u32::from(property.array_dim).max(1) {
            slots.push(Slot {
                struct_index: index,
                property_index,
                element,
            });
        }
    }

    if let Some(&parent) = entry.super_struct.as_deref().and_then(|n| structs.get(n))
        && parent != index
    {
        flatten_one(inner, structs, parent, done, depth + 1);
        if let Some(inherited) = done.get(parent as usize).and_then(Option::as_ref) {
            slots.extend_from_slice(inherited);
        }
    }

    if let Some(cell) = done.get_mut(index as usize) {
        *cell = Some(slots);
    }
}

/// The Oodle and Brotli paths in the usmap crate are unimplemented and panic, so reject those
/// before parsing. Mirrors the private header layout in that crate.
fn check_supported_compression(bytes: &[u8]) -> Result<(), String> {
    let mut cursor = crate::reader::Cursor::new(bytes, 0);
    let magic = cursor
        .read_u16()
        .map_err(|_| "file is too short to be a .usmap".to_string())?;
    if magic != 0x30C4 {
        return Err(format!("not a .usmap file (magic 0x{magic:04X})"));
    }
    let version = cursor.read_u8()?;
    if version >= 1 && cursor.read_i32()? > 0 {
        cursor.skip(8)?;
        let custom_versions = cursor.read_u32()?;
        for _ in 0..custom_versions {
            cursor.skip(24)?;
        }
        cursor.skip(4)?;
    }
    match cursor.read_u8()? {
        0 | 3 => Ok(()),
        1 => Err(oodle_or_brotli("Oodle")),
        2 => Err(oodle_or_brotli("Brotli")),
        other => Err(format!("unknown .usmap compression method {other}")),
    }
}

fn oodle_or_brotli(method: &str) -> String {
    format!(
        "this .usmap is {method} compressed, which is not supported. Re-save it uncompressed or zstd compressed."
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use usmap::{PropertyInner, Struct};

    fn property(name: &str, index: u16, array_dim: u8) -> Property {
        Property {
            name: name.to_string(),
            array_dim,
            index,
            inner: PropertyInner::Int,
        }
    }

    fn mappings(structs: Vec<Struct>) -> Mappings {
        Mappings::from_usmap(Usmap {
            enums: Vec::new(),
            structs,
            cext: None,
            ppth: None,
            eatr: None,
            envp: None,
        })
    }

    #[test]
    fn temp_scan_identical_layouts() {
        let Ok(path) = std::env::var("RIVALS_USMAP") else {
            return;
        };
        if std::env::var("RIVALS_SCAN_LAYOUTS").is_err() {
            return;
        }
        let bytes = std::fs::read(&path).expect("usmap");
        let mappings = Mappings::load(&bytes).expect("load");
        let names: Vec<String> = mappings.structs.keys().cloned().collect();
        let mut by_layout: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let mut actors = 0usize;
        for name in &names {
            if !mappings.inherits_from(name, "Actor") {
                continue;
            }
            actors += 1;
            let Some(schema) = mappings.schema(name) else {
                continue;
            };
            let key: String = (0..schema.len())
                .filter_map(|at| schema.slot(at))
                .map(|slot| {
                    format!(
                        "{}#{}#{};",
                        slot.property.name,
                        slot.element,
                        kind_name(&slot.property.inner)
                    )
                })
                .collect();
            by_layout.entry(key).or_default().push(name.clone());
        }
        let mut groups: Vec<&Vec<String>> =
            by_layout.values().filter(|held| held.len() > 1).collect();
        groups.sort_by_key(|held| std::cmp::Reverse(held.len()));
        let shared: usize = groups.iter().map(|held| held.len()).sum();
        println!("actor-derived classes in the mappings: {actors}");
        println!("distinct layouts: {}", by_layout.len());
        println!(
            "classes sharing a layout with another: {shared} in {} groups",
            groups.len()
        );
        for group in groups.iter().take(8) {
            println!(
                "  {} classes: {:?}",
                group.len(),
                &group[..group.len().min(6)]
            );
        }
        // What shares a layout with the practice range's own level blueprint?
        for wanted in ["PracticeRange_C", "MarvelEmoteBasketBallHoop_BP_C"] {
            match by_layout
                .values()
                .find(|held| held.iter().any(|n| n == wanted))
            {
                Some(group) => println!(
                    "  {wanted}: shares a layout with {} others, e.g. {:?}",
                    group.len() - 1,
                    group
                        .iter()
                        .filter(|n| *n != wanted)
                        .take(6)
                        .collect::<Vec<_>>()
                ),
                None => println!("  {wanted}: no group (not actor-derived or no schema)"),
            }
        }
    }

    #[test]
    fn a_structs_own_properties_come_before_the_ones_it_inherits() {
        let m = mappings(vec![
            Struct {
                name: "Parent".into(),
                super_struct: None,
                properties: vec![property("FromParent", 0, 1)],
            },
            Struct {
                name: "Child".into(),
                super_struct: Some("Parent".into()),
                properties: vec![property("FromChild", 0, 1)],
            },
        ]);
        let schema = m.schema("Child").expect("schema");
        let names: Vec<_> = schema.iter().map(|s| s.property.name.clone()).collect();
        assert_eq!(names, ["FromChild", "FromParent"]);
    }

    fn three_slots() -> Mappings {
        mappings(vec![Struct {
            name: "S".into(),
            super_struct: None,
            properties: vec![
                property("A", 0, 1),
                property("B", 1, 1),
                property("C", 2, 1),
                property("D", 3, 1),
            ],
        }])
    }

    /// A header index addresses the build's schema, so eliding a slot has to renumber every slot
    /// above it. Getting this wrong shifts every property after the elision into its neighbour.
    #[test]
    fn eliding_a_slot_renumbers_the_ones_above_it() {
        let m = three_slots();
        let fixups = SchemaFixups::one("S", 1);
        let schema = m.schema_fixed("S", Some(&fixups)).expect("schema");
        assert_eq!(schema.len(), 3);
        let names: Vec<_> = schema.iter().map(|s| s.property.name.clone()).collect();
        assert_eq!(names, ["A", "C", "D"]);
    }

    #[test]
    fn eliding_several_slots_renumbers_by_the_count_below_each_index() {
        let m = three_slots();
        let mut fixups = SchemaFixups::one("S", 0);
        fixups.add("S", 2);
        let schema = m.schema_fixed("S", Some(&fixups)).expect("schema");
        let names: Vec<_> = schema.iter().map(|s| s.property.name.clone()).collect();
        assert_eq!(names, ["B", "D"]);
    }

    /// Fixups are keyed by struct so a repair found for one never silently shifts another.
    #[test]
    fn a_fixup_only_applies_to_the_struct_it_names() {
        let m = three_slots();
        let fixups = SchemaFixups::one("Other", 1);
        let schema = m.schema_fixed("S", Some(&fixups)).expect("schema");
        assert_eq!(schema.len(), 4);
    }

    #[test]
    fn adding_the_same_slot_twice_elides_it_once() {
        let mut fixups = SchemaFixups::one("S", 2);
        fixups.add("S", 2);
        assert_eq!(fixups.get("S"), [2]);
    }

    /// Real dumps repeat names: one entry per Blueprint for generated types, plus stubs whose
    /// super points at their own name. Taking whichever came last leaves an empty schema, and the
    /// export then fails on a slot the file appears not to declare.
    #[test]
    fn a_repeated_name_resolves_to_the_fullest_entry_that_describes_itself() {
        let m = mappings(vec![
            Struct {
                name: "Cue_BP_C".into(),
                super_struct: Some("CueBase".into()),
                properties: vec![property("A", 0, 1), property("B", 1, 1)],
            },
            Struct {
                name: "Cue_BP_C".into(),
                super_struct: Some("Cue_BP_C".into()),
                properties: vec![],
            },
            Struct {
                name: "Cue_BP_C".into(),
                super_struct: Some("CueBase".into()),
                properties: vec![property("A", 0, 1)],
            },
        ]);
        let schema = m.schema("Cue_BP_C").expect("schema");
        let names: Vec<_> = schema.iter().map(|s| s.property.name.clone()).collect();
        assert_eq!(names, ["A", "B"]);
    }

    /// A `.usmap` keys classes and structs by the same prefix-stripped name, so a class the file
    /// omits resolves to a struct that happens to share it. Decoding an export against that struct
    /// yields plausible-looking nonsense rather than an error, so the lookup has to reject it.
    #[test]
    fn an_export_class_never_resolves_to_a_same_named_struct() {
        let m = mappings(vec![
            Struct {
                name: "Object".into(),
                super_struct: None,
                properties: vec![],
            },
            Struct {
                name: "DataTable".into(),
                super_struct: Some("Object".into()),
                properties: vec![property("RowStruct", 0, 1)],
            },
            Struct {
                name: "DamageEvent".into(),
                super_struct: None,
                properties: vec![property("DamageTypeClass", 0, 1)],
            },
            Struct {
                name: "DamageTakenEvent".into(),
                super_struct: Some("DamageEvent".into()),
                properties: vec![],
            },
        ]);
        assert!(m.class_schema("DataTable", None).is_some());
        assert!(m.class_schema("Object", None).is_some());
        assert!(m.class_schema("DamageEvent", None).is_none());
        // A struct that derives from another struct still roots outside Object.
        assert!(m.class_schema("DamageTakenEvent", None).is_none());
        // Struct properties still resolve: the restriction is only on an export's class.
        assert!(m.schema("DamageEvent").is_some());
    }

    #[test]
    fn ancestry_runs_from_the_most_distant_ancestor_down_to_the_class_itself() {
        let m = mappings(vec![
            Struct {
                name: "Object".into(),
                super_struct: None,
                properties: vec![],
            },
            Struct {
                name: "ActorComponent".into(),
                super_struct: Some("Object".into()),
                properties: vec![],
            },
            Struct {
                name: "SceneComponent".into(),
                super_struct: Some("ActorComponent".into()),
                properties: vec![],
            },
        ]);
        assert_eq!(
            m.ancestry("SceneComponent"),
            ["Object", "ActorComponent", "SceneComponent"]
        );
    }

    #[test]
    fn a_subclass_is_recognised_as_inheriting_from_its_ancestor() {
        let m = mappings(vec![
            Struct {
                name: "DataTable".into(),
                super_struct: None,
                properties: vec![],
            },
            Struct {
                name: "MarvelTable".into(),
                super_struct: Some("DataTable".into()),
                properties: vec![],
            },
        ]);
        assert!(m.inherits_from("MarvelTable", "DataTable"));
        assert!(m.inherits_from("DataTable", "DataTable"));
        assert!(!m.inherits_from("DataTable", "MarvelTable"));
    }

    #[test]
    fn a_static_array_property_occupies_one_slot_per_element() {
        let m = mappings(vec![Struct {
            name: "S".into(),
            super_struct: None,
            properties: vec![property("Cooldowns", 0, 3), property("After", 3, 1)],
        }]);
        let schema = m.schema("S").expect("schema");
        assert_eq!(schema.len(), 4);
        assert_eq!(schema.slot(2).expect("slot").element, 2);
        assert_eq!(schema.slot(3).expect("slot").property.name, "After");
    }

    #[test]
    fn properties_are_ordered_by_declared_index_not_by_file_order() {
        let m = mappings(vec![Struct {
            name: "S".into(),
            super_struct: None,
            properties: vec![property("Second", 1, 1), property("First", 0, 1)],
        }]);
        let schema = m.schema("S").expect("schema");
        let names: Vec<_> = schema.iter().map(|s| s.property.name.clone()).collect();
        assert_eq!(names, ["First", "Second"]);
    }

    #[test]
    fn a_cyclic_super_struct_chain_terminates_instead_of_recursing_forever() {
        let m = mappings(vec![
            Struct {
                name: "A".into(),
                super_struct: Some("B".into()),
                properties: vec![property("FromA", 0, 1)],
            },
            Struct {
                name: "B".into(),
                super_struct: Some("A".into()),
                properties: vec![property("FromB", 0, 1)],
            },
        ]);
        assert!(!m.schema("A").expect("schema").is_empty());
    }

    #[test]
    fn an_oodle_compressed_mappings_file_is_rejected_with_a_readable_message() {
        let mut bytes = 0x30C4u16.to_le_bytes().to_vec();
        bytes.push(0);
        bytes.push(1);
        let err = check_supported_compression(&bytes).expect_err("should reject");
        assert!(err.contains("Oodle"), "{err}");
    }

    #[test]
    fn a_version_four_header_with_no_package_versioning_finds_the_compression_byte() {
        let mut bytes = 0x30C4u16.to_le_bytes().to_vec();
        bytes.push(4);
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.push(3);
        assert!(check_supported_compression(&bytes).is_ok());
    }
}

/// Set `RIVALS_USMAP` to a real mappings file to run these. They are skipped otherwise because
/// the repo ships no fixtures, and they assert the shape assumptions the parser depends on.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod real_mappings_tests {
    use super::*;

    fn load_real() -> Option<Mappings> {
        let bytes = std::fs::read(std::env::var("RIVALS_USMAP").ok()?).ok()?;
        Mappings::load(&bytes).ok()
    }

    fn plain(name: &str, super_struct: Option<&str>, properties: usize) -> Struct {
        Struct {
            name: name.into(),
            super_struct: super_struct.map(str::to_string),
            properties: (0..properties)
                .map(|i| Property {
                    name: format!("P{i}"),
                    array_dim: 1,
                    index: i as u16,
                    inner: PropertyInner::Int,
                })
                .collect(),
        }
    }

    /// Two entries of one name resolve to the fuller one; the other is a twin a caller can switch
    /// to, which re-flattens every chain through the name. A synthesised set rebased onto the
    /// switched mappings keeps only what it recovered itself.
    #[test]
    fn a_twin_is_chosen_by_fullness_and_can_be_switched() {
        let mappings = Mappings::from_structs(vec![
            plain("Object", None, 0),
            plain("X", Some("Object"), 2),
            plain("Y", Some("Object"), 1),
            plain("A", Some("X"), 0),
            plain("A", Some("Y"), 1),
        ]);
        assert_eq!(mappings.twins("A"), &[3, 4]);
        assert_eq!(mappings.chosen("A"), Some(4));
        assert_eq!(mappings.ancestry("A"), vec!["Object", "Y", "A"]);
        assert_eq!(mappings.schema("A").expect("schema").len(), 2);
        assert!(mappings.twins("X").is_empty());

        let switched = mappings.with_twin("A", 3);
        assert_eq!(switched.chosen("A"), Some(3));
        assert_eq!(switched.ancestry("A"), vec!["Object", "X", "A"]);
        assert_eq!(switched.schema("A").expect("schema").len(), 2);
        assert_eq!(switched.twins("A"), &[3, 4], "the twins stay known");

        let synth = Mappings::from_structs(vec![
            plain("Z", Some("A"), 1),
            plain("A", Some("Y"), 1),
            plain("Y", Some("Object"), 1),
            plain("Object", None, 0),
        ]);
        let recovered = synth.recovered(&switched);
        assert_eq!(
            recovered
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Z"],
            "the copied chain is not recovered, whichever twin it copied"
        );
    }

    #[test]
    fn a_deep_class_flattens_to_the_sum_of_every_link_in_its_chain() {
        let Some(m) = load_real() else {
            return;
        };
        let mut name = Some("StaticMeshComponent".to_string());
        let mut expected = 0usize;
        while let Some(current) = name {
            let Some(&index) = m.structs.get(&current) else {
                break;
            };
            let entry = &m.inner.structs[index as usize];
            expected += entry
                .properties
                .iter()
                .map(|p| usize::from(p.array_dim).max(1))
                .sum::<usize>();
            name = entry.super_struct.clone();
        }
        assert_eq!(
            m.schema("StaticMeshComponent").expect("schema").len(),
            expected
        );
    }

    #[test]
    fn property_indices_are_local_to_their_own_struct_not_global() {
        let Some(m) = load_real() else {
            return;
        };
        for name in ["StaticMeshComponent", "MeshComponent", "PrimitiveComponent"] {
            let index = m.structs.get(name).copied().expect("struct present");
            let entry = &m.inner.structs[index as usize];
            assert_eq!(
                entry.properties.iter().map(|p| p.index).min(),
                Some(0),
                "{name} should index from zero"
            );
        }
    }

    #[test]
    fn the_most_derived_properties_are_reached_before_the_inherited_ones() {
        let Some(m) = load_real() else {
            return;
        };
        let schema = m.schema("StaticMeshComponent").expect("schema");
        let own = m
            .structs
            .get("StaticMeshComponent")
            .map(|i| m.inner.structs[*i as usize].properties.len())
            .expect("own properties");
        assert!(own > 0);
        assert_eq!(
            schema.slot(0).expect("first slot").owner,
            "StaticMeshComponent"
        );
    }

    /// Types absent from the mappings are unreachable, which is why their decoders are allowed
    /// to be missing. If a future mappings file introduces one, this fails rather than letting
    /// unverified code run against real assets for the first time in production.
    #[test]
    fn the_property_types_this_crate_declines_to_decode_are_absent_from_the_mappings() {
        let Some(m) = load_real() else {
            return;
        };
        let counts = m.property_kind_counts();
        for kind in ["Optional", "Utf8Str", "AnsiStr", "Unknown"] {
            assert_eq!(
                counts.get(kind),
                None,
                "{kind} now appears in the mappings, so its decoder has to be written and tested"
            );
        }
    }

    #[test]
    fn the_property_types_that_are_decoded_from_unconfirmed_layouts_are_still_rare() {
        let Some(m) = load_real() else {
            return;
        };
        let counts = m.property_kind_counts();
        assert!(
            counts.get("Delegate").is_some_and(|n| *n > 0),
            "delegates are declared, so the layout pinned in props.rs is load bearing"
        );
    }

    /// Prints the flattened schema for the struct named by `RIVALS_STRUCT`. Not an assertion:
    /// this is the tool for lining a schema up against a byte stream when an export desyncs.
    /// Prints what the loaded mappings file actually contains. `.usmap` can carry optional
    /// extension blocks, and property flags in particular would settle cases the reader currently
    /// has to infer from the bytes, so it is worth knowing whether a dump includes them.
    #[test]
    fn dump_mappings_summary() {
        let (Some(m), Ok(_)) = (load_real(), std::env::var("RIVALS_SUMMARY")) else {
            return;
        };
        println!("structs {}", m.struct_count());
        println!("enums {}", m.enum_count());
        println!("cext (class extensions) {}", m.inner.cext.is_some());
        println!("ppth (property paths)   {}", m.inner.ppth.is_some());
        println!("eatr (property flags)   {}", m.inner.eatr.is_some());
        println!("envp (enum value pairs) {}", m.inner.envp.is_some());
        let rooted = m
            .inner
            .structs
            .iter()
            .filter(|s| m.ancestry(&s.name).first() == Some(&"Object"))
            .count();
        println!("entries rooted at Object (classes) {rooted}");
        println!(
            "entries not rooted at Object (structs) {}",
            m.struct_count() - rooted
        );
    }

    /// Lists every entry whose name contains `RIVALS_GREP`, which is how to tell whether a dump
    /// covers a family of types or missed it because that content was never loaded.
    #[test]
    fn dump_matching_names() {
        let (Some(m), Ok(needle)) = (load_real(), std::env::var("RIVALS_GREP")) else {
            return;
        };
        let needle = needle.to_lowercase();
        for entry in &m.inner.structs {
            if entry.name.to_lowercase().contains(&needle) {
                println!(
                    "NAME {:<58} super={:?} properties={}",
                    entry.name,
                    entry.super_struct,
                    entry.properties.len()
                );
            }
        }
    }

    #[test]
    fn dump_schema() {
        let (Some(m), Ok(name)) = (load_real(), std::env::var("RIVALS_STRUCT")) else {
            return;
        };
        for (index, entry) in m.inner.structs.iter().enumerate() {
            if entry.name == name {
                println!(
                    "ENTRY {index} super={:?} own_properties={}",
                    entry.super_struct,
                    entry.properties.len()
                );
                let flags = m
                    .inner
                    .eatr
                    .as_ref()
                    .and_then(|e| e.struct_flags.get(index));
                if let Some(flags) = flags {
                    for (i, property) in entry.properties.iter().enumerate() {
                        println!(
                            "FLAGS idx={:<4} {:<46} {:#018x}",
                            property.index,
                            property.name,
                            flags.prop_flags.get(i).copied().unwrap_or_default()
                        );
                    }
                }
            }
        }
        let Some(schema) = m.schema(&name) else {
            println!("MISSING {name}");
            return;
        };
        for (i, slot) in schema.iter().enumerate() {
            println!(
                "SLOT {i:3} idx={:<4} dim={:<3} {:<44} owner={:<24} {:?}",
                slot.property.index,
                slot.property.array_dim,
                slot.property.name,
                slot.owner,
                slot.property.inner
            );
        }
    }

    #[test]
    fn the_game_subclasses_data_table_so_inheritance_has_to_be_followed() {
        let Some(m) = load_real() else {
            return;
        };
        assert!(m.inherits_from("CompositeDataTable", "DataTable"));
    }
}
