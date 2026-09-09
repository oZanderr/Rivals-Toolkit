//! Reads the Niagara variable structs, which serialize themselves around a schema-driven type
//! block instead of through their own property headers.
//!
//! `FNiagaraVariableBase` writes its name, then its type handle. The handle's serializer emits the
//! `FNiagaraTypeDefinition` it points at as an ordinary unversioned property block, so that part
//! still comes from the mappings. `FNiagaraVariable` follows with its raw value bytes and
//! `FNiagaraVariableWithOffset` with its offset, again without a header. The mappings declare a
//! converter struct after the offset that this build does not write. A GPU script's data interface
//! parameter info and its generated functions are the same kind of thing: strings, names and
//! counted lists with no header, and none of the shader parameter offset the mappings declare.
//! Each list is recorded as a container, so it grows and shrinks like a declared array.

use crate::props::{
    ContainerLayout, Ctx, DefaultPart, Diagnostics, native_list, read_index, read_struct, spanned,
};
use crate::reader::Cursor;
use crate::structs::NativeDefault;
use crate::value::{PropertyEntry, PropertyValue};

/// Whether `name` is one of the structs this module reads in place of the schema.
pub(crate) fn reads(name: &str) -> bool {
    matches!(
        name,
        "NiagaraVariableBase"
            | "NiagaraVariable"
            | "NiagaraVariableWithOffset"
            | "NiagaraTypeDefinitionHandle"
            | "NiagaraDataInterfaceGPUParamInfo"
            | "NiagaraDataInterfaceGeneratedFunction"
    )
}

/// What each layout holds when default-constructed: the name `None`, an empty type definition
/// block, and empty strings, lists and data.
pub(crate) fn default_of(name: &str) -> NativeDefault {
    let type_block = DefaultPart::Struct("NiagaraTypeDefinition");
    match name {
        "NiagaraTypeDefinitionHandle" => NativeDefault::Recipe(vec![type_block]),
        "NiagaraVariableBase" => NativeDefault::Recipe(vec![DefaultPart::NoneName, type_block]),
        // An empty `VarData` array, or a zero offset: four zero bytes either way.
        "NiagaraVariable" | "NiagaraVariableWithOffset" => NativeDefault::Recipe(vec![
            DefaultPart::NoneName,
            type_block,
            DefaultPart::Bytes(vec![0u8; 4]),
        ]),
        "NiagaraDataInterfaceGPUParamInfo" => NativeDefault::Fixed(vec![0u8; 12]),
        "NiagaraDataInterfaceGeneratedFunction" => NativeDefault::Recipe(function_recipe()),
        _ => NativeDefault::NotNative,
    }
}

/// A function named None with an empty instance name and no specifiers or variadic parameters.
fn function_recipe() -> Vec<DefaultPart> {
    vec![DefaultPart::NoneName, DefaultPart::Bytes(vec![0u8; 16])]
}

pub(crate) fn read(
    name: &str,
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    match name {
        "NiagaraTypeDefinitionHandle" => return type_definition(cursor, ctx, diagnostics, depth),
        "NiagaraDataInterfaceGPUParamInfo" => return gpu_param_info(cursor, ctx, diagnostics),
        "NiagaraDataInterfaceGeneratedFunction" => {
            return generated_function(cursor, ctx, diagnostics);
        }
        _ => {}
    }
    let mut fields = vec![
        spanned(cursor, "Name", |c| {
            Ok(PropertyValue::Name {
                value: c.read_name(ctx.names())?,
            })
        })?,
        spanned(cursor, "TypeDefHandle", |c| {
            type_definition(c, ctx, diagnostics, depth)
        })?,
    ];
    match name {
        "NiagaraVariable" => fields.push(var_data(cursor, diagnostics)?),
        "NiagaraVariableWithOffset" => {
            fields.push(spanned(cursor, "Offset", |c| {
                Ok(PropertyValue::Int {
                    value: i64::from(c.read_i32()?),
                })
            })?);
        }
        _ => {}
    }
    Ok(PropertyValue::Struct {
        name: name.to_string(),
        fields,
    })
}

/// `FNiagaraTypeDefinitionHandle::Serialize` writes the definition itself rather than the
/// registry index the mappings declare.
fn type_definition(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
    depth: u32,
) -> Result<PropertyValue, String> {
    read_struct("NiagaraTypeDefinition", cursor, ctx, diagnostics, depth + 1)
}

/// `FNiagaraDataInterfaceGPUParamInfo::Serialize`: two strings, then the generated functions as
/// an array whose empty element is a None-named function with no specifiers or parameters.
fn gpu_param_info(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let fields = vec![
        spanned(cursor, "DataInterfaceHLSLSymbol", string)?,
        spanned(cursor, "DIClassName", string)?,
        native_list(
            cursor,
            diagnostics,
            "GeneratedFunctions",
            function_recipe(),
            |c, diagnostics| generated_function(c, ctx, diagnostics),
        )?,
    ];
    Ok(PropertyValue::Struct {
        name: "NiagaraDataInterfaceGPUParamInfo".to_string(),
        fields,
    })
}

/// `FNiagaraDataInterfaceGeneratedFunction::Serialize`: the definition and instance names, the
/// specifier pairs, then the variadic inputs and outputs. Each list is a count and its elements,
/// present even when empty so it can be grown.
fn generated_function(
    cursor: &mut Cursor<'_>,
    ctx: &Ctx<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyValue, String> {
    let mut fields = vec![
        spanned(cursor, "DefinitionName", |c| name(c, ctx))?,
        spanned(cursor, "InstanceName", string)?,
        native_list(
            cursor,
            diagnostics,
            "Specifiers",
            vec![DefaultPart::NoneName, DefaultPart::NoneName],
            |c, _| {
                Ok(PropertyValue::Struct {
                    name: "NiagaraFunctionSpecifier".to_string(),
                    fields: vec![
                        spanned(c, "Key", |c| name(c, ctx))?,
                        spanned(c, "Value", |c| name(c, ctx))?,
                    ],
                })
            },
        )?,
    ];
    for list in ["VariadicInputs", "VariadicOutputs"] {
        fields.push(native_list(
            cursor,
            diagnostics,
            list,
            vec![DefaultPart::NoneName, DefaultPart::Bytes(vec![0u8; 4])],
            |c, diagnostics| {
                Ok(PropertyValue::Struct {
                    name: "NiagaraVariableCommonReference".to_string(),
                    fields: vec![
                        spanned(c, "Name", |c| name(c, ctx))?,
                        spanned(c, "UnderlyingType", |c| {
                            let index = read_index(c, diagnostics)?;
                            Ok(PropertyValue::Object {
                                index,
                                path: ctx.object_path(index).map_err(|e| c.err(e))?,
                            })
                        })?,
                    ],
                })
            },
        )?);
    }
    Ok(PropertyValue::Struct {
        name: "NiagaraDataInterfaceGeneratedFunction".to_string(),
        fields,
    })
}

fn string(cursor: &mut Cursor<'_>) -> Result<PropertyValue, String> {
    Ok(PropertyValue::Str {
        value: cursor.read_string()?,
    })
}

fn name(cursor: &mut Cursor<'_>, ctx: &Ctx<'_>) -> Result<PropertyValue, String> {
    Ok(PropertyValue::Name {
        value: cursor.read_name(ctx.names())?,
    })
}

/// `TArray<uint8> VarData`, recorded as a container so the bytes stay inspectable and editable.
fn var_data(
    cursor: &mut Cursor<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<PropertyEntry, String> {
    let start = cursor.file_offset();
    let count = cursor.read_i32()?;
    if count < 0 || count as usize > cursor.remaining() {
        return Err(cursor.err(format!("implausible Niagara variable data length {count}")));
    }
    let mut items = Vec::with_capacity(count as usize);
    let mut elements = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let at = cursor.file_offset();
        items.push(PropertyValue::Byte {
            value: cursor.read_u8()?,
        });
        elements.push((at, cursor.file_offset()));
    }
    diagnostics.containers.push(ContainerLayout {
        at: start,
        count_at: start,
        count_width: 4,
        elements,
        element_kind: "Byte",
        default_element: Some(vec![0]),
        element_is_enum: false,
        element_enum: None,
        default_name: None,
        default_recipe: None,
        keys: None,
    });
    Ok(PropertyEntry {
        name: "VarData".to_string(),
        element: None,
        value: PropertyValue::Array { items },
        span: Some((start, cursor.file_offset())),
        slot: None,
    })
}

/// A field whose bytes are its own, recorded so it can be edited in place later.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use retoc::legacy_asset::{FLegacyPackageHeader, FPackageNameMap};
    use usmap::{Property, PropertyInner, Struct};

    use super::*;
    use crate::mappings::Mappings;

    fn property(name: &str, index: u16, inner: PropertyInner) -> Property {
        Property {
            name: name.into(),
            array_dim: 1,
            index,
            inner,
        }
    }

    /// The two reflected structs the composites lean on, shaped like the mappings declare them.
    fn mappings() -> Mappings {
        Mappings::from_structs(vec![Struct {
            name: "NiagaraTypeDefinition".into(),
            super_struct: None,
            properties: vec![
                property("ClassStructOrEnum", 0, PropertyInner::Object),
                property("UnderlyingType", 1, PropertyInner::UInt16),
                property("Flags", 2, PropertyInner::Byte),
            ],
        }])
    }

    fn header() -> FLegacyPackageHeader {
        FLegacyPackageHeader {
            name_map: FPackageNameMap::create_from_names(vec![
                "None".into(),
                "Emitter.RandomSeed".into(),
            ]),
            ..Default::default()
        }
    }

    /// The 17-byte shape measured in the game: a name, then the type block `80 07 04` with the
    /// third slot zero, an object index and the underlying type.
    fn base_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&[0x80, 0x07, 0x04]);
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out
    }

    fn read_all(name: &str, data: &[u8]) -> (PropertyValue, usize) {
        let (value, consumed, _) = read_recording(name, data);
        (value, consumed)
    }

    fn read_recording(name: &str, data: &[u8]) -> (PropertyValue, usize, Vec<ContainerLayout>) {
        let mappings = mappings();
        let header = header();
        let ctx = Ctx {
            mappings: Some(&mappings),
            header: &header,
            fixups: None,
            synth: None,
            local: None,
        };
        let mut diagnostics = Diagnostics::default();
        let mut cursor = Cursor::new(data, 0x100);
        let value = read(name, &mut cursor, &ctx, &mut diagnostics, 0).expect(name);
        (value, cursor.position(), diagnostics.containers)
    }

    #[test]
    fn a_variable_base_is_a_name_and_a_type_block_with_no_header_of_its_own() {
        let (value, consumed) = read_all("NiagaraVariableBase", &base_bytes());
        assert_eq!(consumed, 17);
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        assert_eq!(fields.len(), 2);
        assert!(
            matches!(&fields[0].value, PropertyValue::Name { value } if value == "Emitter.RandomSeed")
        );
        assert_eq!(fields[0].span, Some((0x100, 0x108)));
        let PropertyValue::Struct {
            name,
            fields: inner,
        } = &fields[1].value
        else {
            panic!("expected the type definition");
        };
        assert_eq!(name, "NiagaraTypeDefinition");
        assert_eq!(inner[1].value.summary(), "2");
        // The third slot is zero-masked: a typed zero occupying no bytes.
        assert_eq!(inner[2].span.map(|(start, end)| end - start), Some(0));
        assert_eq!(inner[2].value.summary(), "0");
    }

    #[test]
    fn a_variable_adds_its_raw_value_bytes_as_a_container() {
        let mut data = base_bytes();
        data.extend_from_slice(&2i32.to_le_bytes());
        data.extend_from_slice(&[0xAA, 0xBB]);
        let (value, consumed) = read_all("NiagaraVariable", &data);
        assert_eq!(consumed, data.len());
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        assert_eq!(fields[2].name, "VarData");
        assert!(matches!(&fields[2].value, PropertyValue::Array { items } if items.len() == 2));
        assert_eq!(fields[2].span, Some((0x111, 0x117)));
    }

    fn write_string(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&(text.len() as i32 + 1).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    }

    fn write_name(out: &mut Vec<u8>, index: i32) {
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
    }

    /// The shape measured in a GPU script: no header anywhere, every list a count and its items,
    /// three counts after each function's instance name and nothing after the last function.
    #[test]
    fn a_gpu_param_info_is_strings_and_counted_functions_without_a_header() {
        let mut data = Vec::new();
        write_string(&mut data, "WindForce_LandscapeSample");
        write_string(&mut data, "NiagaraDataInterfaceLandscape");
        data.extend_from_slice(&2i32.to_le_bytes());
        write_name(&mut data, 1);
        write_string(&mut data, "GetHeight_WindForce_LandscapeSample");
        data.extend_from_slice(&1i32.to_le_bytes());
        write_name(&mut data, 1);
        write_name(&mut data, 0);
        data.extend_from_slice(&1i32.to_le_bytes());
        write_name(&mut data, 1);
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        write_name(&mut data, 0);
        write_string(&mut data, "GetWorldNormal_WindForce_LandscapeSample");
        data.extend_from_slice(&[0u8; 12]);

        let (value, consumed, containers) =
            read_recording("NiagaraDataInterfaceGPUParamInfo", &data);
        assert_eq!(consumed, data.len());
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        let names: Vec<_> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "DataInterfaceHLSLSymbol",
                "DIClassName",
                "GeneratedFunctions"
            ]
        );
        let PropertyValue::Array { items: functions } = &fields[2].value else {
            panic!("expected the functions as an array");
        };
        assert_eq!(functions.len(), 2);
        let PropertyValue::Struct { fields: first, .. } = &functions[0] else {
            panic!("expected a function");
        };
        let first_names: Vec<_> = first.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            first_names,
            [
                "DefinitionName",
                "InstanceName",
                "Specifiers",
                "VariadicInputs",
                "VariadicOutputs"
            ]
        );
        assert!(
            matches!(&first[0].value, PropertyValue::Name { value } if value == "Emitter.RandomSeed")
        );
        assert_eq!(
            first[2].span.map(|(s, e)| e - s),
            Some(4 + 16),
            "a count and a pair of names"
        );
        assert_eq!(
            first[3].span.map(|(s, e)| e - s),
            Some(4 + 12),
            "a count, a name and an object"
        );
        assert!(
            matches!(&first[4].value, PropertyValue::Array { items } if items.is_empty()),
            "an empty list is still an array, so it can grow"
        );
        let PropertyValue::Struct { fields: second, .. } = &functions[1] else {
            panic!("expected a function");
        };
        assert!(
            second[2..]
                .iter()
                .all(|f| matches!(&f.value, PropertyValue::Array { items } if items.is_empty()))
        );

        // Every list is a container an edit can address: the functions, then per function its
        // specifiers, inputs and outputs. A specifier list grows from empty by two None names.
        assert_eq!(containers.len(), 1 + 2 * 3);
        let functions_layout = containers
            .iter()
            .find(|c| c.at == fields[2].span.expect("span").0)
            .expect("the functions container");
        assert_eq!(functions_layout.elements.len(), 2);
        assert_eq!(
            functions_layout.default_recipe,
            Some(vec![
                DefaultPart::NoneName,
                DefaultPart::Bytes(vec![0u8; 16])
            ])
        );
        let specifiers_layout = containers
            .iter()
            .find(|c| c.at == first[2].span.expect("span").0)
            .expect("the specifiers container");
        assert_eq!(
            specifiers_layout.default_recipe,
            Some(vec![DefaultPart::NoneName, DefaultPart::NoneName])
        );
    }

    /// The 21-byte stride measured in a parameter store: the base, then the offset alone.
    #[test]
    fn a_variable_with_offset_adds_only_the_offset() {
        let mut data = base_bytes();
        data.extend_from_slice(&7i32.to_le_bytes());
        let (value, consumed) = read_all("NiagaraVariableWithOffset", &data);
        assert_eq!(consumed, 21);
        let PropertyValue::Struct { fields, .. } = value else {
            panic!("expected a struct");
        };
        assert_eq!(fields.len(), 3);
        assert!(matches!(fields[2].value, PropertyValue::Int { value: 7 }));
    }
}
