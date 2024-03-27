// This is a hacked up copy/paste of the Gimli example, modified to do stuff.
// That file doesn't have a copyright notice, but I think the whole of gimli is
//     (C) The Rust Project Developers

use std::{env, fs};

use anyhow::{anyhow, Context, Result};
use fallible_iterator::FallibleIterator;
use itertools::Itertools;
use object::{Object, ObjectKind};
use typed_arena::Arena;

mod reloc;

#[derive(Debug, Clone, serde::Serialize)]
struct CodeRange {
    start: u64,
    end: u64,
}

impl CodeRange {
    /// The extent of `entry` as a CodeRange
    fn from_function_die<T: gimli::Reader>(
        entry: &gimli::DebuggingInformationEntry<T>,
    ) -> Result<Option<Self>> {
        assert!(entry.tag() == gimli::DW_TAG_subprogram);

        let low = match entry.attr_value(gimli::DW_AT_low_pc)? {
            Some(gimli::AttributeValue::Addr(x)) => x,
            Some(y) => return Err(anyhow!("unknown kind of low_pc: {y:?}")),
            None => return Ok(None),
        };

        let high = match entry.attr_value(gimli::DW_AT_high_pc)? {
            Some(gimli::AttributeValue::Addr(x)) => x,
            Some(y) => return Err(anyhow!("unknown kind of high_pc: {y:?}")),
            None => return Ok(None),
        };

        Ok(Some(CodeRange {
            start: low,
            end: high,
        }))
    }
}

/// A simple description of an offset from a base register, as used for local
/// variables
#[derive(Debug, Clone)]
struct BaseOffset {
    valid: Vec<CodeRange>,
    offset: i64,
    register: gimli::Register,
}

impl BaseOffset {
    /// Given a frame_base attribute, return a suitable BaseOffset
    fn from_frame_base<T: gimli::Reader>(
        frame_base: gimli::AttributeValue<T>,
        range: &CodeRange, // Extent of the function with this attribute
        object: &object::File,
        dwarf: &gimli::Dwarf<T>,
        unit: &gimli::Unit<T>,
    ) -> Result<Option<BaseOffset>> {
        let function_start_pc = range.start;

        let mut bases = match frame_base {
            gimli::AttributeValue::LocationListsRef(ll) => {
                let list = loclist_as_offsets(ll, dwarf, unit)?;

                let pointer_reg = match object.architecture() {
                    object::Architecture::X86_64 => gimli::Register(6), // %rbp
                    object::Architecture::Aarch64 => gimli::Register(31), // %sp
                    x => return Err(anyhow!("unknown architecture {x:?}")),
                };

                // This is the value of the assignment, remember that
                BaseOffset::from_merged_base_offsets(pointer_reg, object, list)
                    .context("merging frame-bases")?
            }
            gimli::AttributeValue::Exprloc(x) => {
                // We see these, but is exceptionally unlikely to see one that
                // also has saved arguments, because this implies the base is
                // constant (and thus if it's the frame pointer, it's the
                // _callers_).
                Some(BaseOffset::from_exprloc(x, range, unit)?)
            }
            x => return Err(anyhow!("unexpected frame-bases: {x:?}")),
        };

        if let Some(x) = bases.as_mut() {
            x.relativize(function_start_pc);
        }

        Ok(bases)
    }

    /// given a DWARF location expression turn it into a BaseOffset
    fn from_exprloc<T: gimli::Reader>(
        expr: gimli::Expression<T>,
        range: &CodeRange,
        unit: &gimli::Unit<T>,
    ) -> Result<Self> {
        let mut ops = expr.operations(unit.encoding());
        let mut bo: Option<BaseOffset> = None;

        if let Some(op) = ops.next()? {
            match op {
                gimli::Operation::RegisterOffset {
                    register,
                    offset,
                    base_type: _,
                } => {
                    bo = Some(BaseOffset {
                        valid: vec![range.clone()],
                        offset,
                        register,
                    })
                }
                x => return Err(anyhow!("unexpected frame_base calculation: {x:?}")),
            }
        }

        if ops.count()? != 0 {
            return Err(anyhow!("base expression has multiple location operations"));
        }

        Ok(bo.unwrap())
    }

    /// If there exists a base offset that is relative to the frame register, or
    /// there exist multiple each with the same offset, that is our offset.
    /// Return a merged entry reflecting all valid pcs.
    ///
    /// XXX: As a sad quirk, on AArch64, if the register is the stack pointer,
    /// ignore 0 offsets.  This is because GCC seems unwilling to _say_ the
    /// frame base is at the frame pointer, on ARM.  So we get entries at SP+0
    /// (which are valid frame bases, but not for us), and at SP+<something>,
    /// which iff equal(something) is ours.
    ///
    /// XXX: We could perhaps figure this out better, but I'm not sure how we
    /// could do it realistically.
    ///
    /// The possibility of multiples come from code generation such as:
    ///   [ prologue ]
    ///   [ work work work ]
    ///   [ epilogue ] <------------.
    ///   [ return ]                |
    ///   [ prologue ]              |
    ///   [ more work work work ]   |
    ///   [ jump to epilogue ] -----'
    ///
    /// which we do see generated
    fn from_merged_base_offsets(
        reg: gimli::Register,
        object: &object::File,
        v: Vec<BaseOffset>,
    ) -> Result<Option<BaseOffset>> {
        let mut it: Box<dyn std::iter::Iterator<Item = BaseOffset>> =
            if object.architecture() == object::Architecture::Aarch64 {
                Box::new(v.into_iter().filter(|x| x.offset != 0))
            } else {
                Box::new(v.into_iter())
            };
        let mut first = it.find(|x| x.register == reg).unwrap();

        // We side-effect `first` to be our return value, and gather each entry
        // that is a register but not an offset match into `bogons`.
        //
        // I don't like this.
        let mut bogons = it
            .filter(|x| x.register == reg)
            .filter_map(|y| {
                if y.offset == first.offset {
                    first.valid.extend(y.valid);
                    None
                } else {
                    Some(y)
                }
            })
            .collect::<Vec<_>>();

        if !bogons.is_empty() {
            bogons.push(first);
            Err(anyhow!("multiple frame-pointer offsets: {bogons:?}"))
        } else {
            Ok(Some(first))
        }
    }

    /// Cause all the validity entries in a BaseOffset to be relative to to, in place
    fn relativize(&mut self, to: u64) {
        self.valid.iter_mut().for_each(|x| {
            x.start -= to;
            x.end -= to;
        });
    }

    /// Take a BaseOffset and a range, and return a vec of CodeRange which covers
    /// the parts of the original the BaseOffset misses.
    ///
    /// range is the more complete range to which the baseoffset is relative
    fn invert(&self, range: &CodeRange) -> Vec<CodeRange> {
        let mut ret: Vec<CodeRange> = Vec::with_capacity(self.valid.len());

        if let Some(x) = self.valid.first() {
            if 0 < x.start {
                ret.push(CodeRange {
                    start: 0,
                    end: x.start - 1,
                });
            }
        }

        for (r1, r2) in self.valid.iter().tuple_windows() {
            if r1.end < r2.start {
                ret.push(CodeRange {
                    start: r1.end,
                    end: r2.start,
                });
            }
        }

        if let Some(x) = self.valid.last() {
            if x.end + 1 < range.end - range.start {
                ret.push(CodeRange {
                    start: x.end + 1,
                    end: range.end - range.start,
                });
            }
        }

        ret
    }
}

/// Return the offset of a given DebuggingInformationEntry
fn entry_to_die_offset<T: gimli::Reader>(
    entry: &gimli::DebuggingInformationEntry<T>,
    unit: &gimli::Unit<T>,
) -> Option<<T as gimli::Reader>::Offset> {
    entry
        .offset()
        .to_debug_info_offset(&unit.header)
        .map(|x| x.0)
}

/// Translate various kinds of DWARF attribute values to strings
fn attr_to_string<T: gimli::Reader>(
    attr: gimli::AttributeValue<T>,
    dwarf: &gimli::Dwarf<T>,
    unit: &gimli::Unit<T>,
) -> Option<String> {
    match attr {
        gimli::AttributeValue::DebugStrRef(_) => dwarf
            .attr_string(unit, attr)
            .unwrap()
            .to_string()
            .ok()
            .map(|x| x.into_owned()),
        gimli::AttributeValue::String(_) => dwarf
            .attr_string(unit, attr)
            .unwrap()
            .to_string()
            .ok()
            .map(|x| x.into_owned()),
        gimli::AttributeValue::FileIndex(n) => {
            let nameref = unit
                .line_program
                .as_ref()
                .and_then(|x| x.header().file(n).map(|f| f.path_name()))
                .unwrap();

            attr_to_string(nameref, dwarf, unit)
        }
        _ => panic!("Unknown attribute for string conversion: {attr:?}"),
    }
}

/// True if this function is concrete, meaning in our terms that is not a
/// prototype, not an abstract parent of an inlined call, and not itself
/// inlined
fn is_concrete_function<T: gimli::Reader>(
    entry: &gimli::DebuggingInformationEntry<T>,
) -> Result<bool> {
    assert!(entry.tag() == gimli::DW_TAG_subprogram);

    if entry.attr_value(gimli::DW_AT_declaration)?.is_some()
        || entry.attr_value(gimli::DW_AT_abstract_origin)?.is_some()
    {
        Ok(false)
    } else {
        match entry.attr_value(gimli::DW_AT_inline)? {
            Some(gimli::AttributeValue::Inline(x)) => match x {
                gimli::DW_INL_inlined => Ok(false),
                gimli::DW_INL_declared_inlined => Ok(false),
                gimli::DW_INL_not_inlined => Ok(true),
                gimli::DW_INL_declared_not_inlined => Ok(true),
                _ => Err(anyhow!("function has weird inline attribute: {x:?}")),
            },
            Some(x) => Err(anyhow!("function has weird inline attribute type: {x:?}")),
            None => Ok(true),
        }
    }
}

/// Source file of the given DebuggingInformationEntry
fn die_source_file<T: gimli::Reader>(
    entry: &gimli::DebuggingInformationEntry<T>,
    dwarf: &gimli::Dwarf<T>,
    unit: &gimli::Unit<T>,
) -> Result<Option<String>> {
    Ok(entry
        .attr_value(gimli::DW_AT_decl_file)?
        .and_then(|x| attr_to_string(x, dwarf, unit)))
}

/// True if a DebuggingInformationEntry is not assembler (we're lax about what "C source" means)
fn die_has_c_source<T: gimli::Reader>(
    entry: &gimli::DebuggingInformationEntry<T>,
    dwarf: &gimli::Dwarf<T>,
    unit: &gimli::Unit<T>,
) -> Result<bool> {
    match die_source_file(entry, dwarf, unit)? {
        Some(file) if file.ends_with(".s") || file.ends_with(".S") => Ok(false),
        Some(_) => Ok(true),
        None => Ok(false), // GAS has no source file names
    }
}

/// Given a DWARF location list, return each as BaseOffsets
fn loclist_as_offsets<T: gimli::Reader>(
    ll: gimli::LocationListsOffset<T::Offset>,
    dwarf: &gimli::Dwarf<T>,
    unit: &gimli::Unit<T>,
) -> Result<Vec<BaseOffset>> {
    let mut locs = dwarf.locations(unit, ll)?;
    let mut vec: Vec<BaseOffset> = Vec::new();

    while let Some(loc) = locs.next()? {
        vec.push(BaseOffset::from_exprloc(
            loc.data,
            &CodeRange {
                start: loc.range.begin,
                end: loc.range.end,
            },
            unit,
        )?);
    }

    Ok(vec)
}

// True if the type of this entry would be passed in an integer register
fn is_register_type<T: gimli::Reader>(
    entry: &gimli::DebuggingInformationEntry<T>,
    unit: &gimli::Unit<T>,
) -> Result<bool> {
    let tipe = match entry.attr_value(gimli::DW_AT_type)? {
        Some(gimli::AttributeValue::UnitRef(x)) => unit.entry(x)?,
        Some(x) => return Err(anyhow!("type has weird value type: {x:?}")),
        None => return Err(anyhow!("entry has no value?")),
    };

    match tipe.tag() {
        gimli::DW_TAG_base_type => match tipe.attr_value(gimli::DW_AT_encoding)? {
            Some(gimli::AttributeValue::Encoding(x)) => match x {
                gimli::DW_ATE_complex_float | gimli::DW_ATE_float => Ok(false),
                gimli::DW_ATE_signed | gimli::DW_ATE_unsigned => Ok(true),
                gimli::DW_ATE_signed_char | gimli::DW_ATE_unsigned_char => Ok(true),
                gimli::DW_ATE_boolean => Ok(true),
                _ => Err(anyhow!("base type has unknown encoding: {x:?}")),
            },
            Some(x) => Err(anyhow!("base type has weird encoding: {x:?}")),
            None => Err(anyhow!("base type has no encoding!")),
        },
        gimli::DW_TAG_pointer_type => Ok(true),
        gimli::DW_TAG_array_type => Ok(true),
        gimli::DW_TAG_enumeration_type => Ok(true),
        gimli::DW_TAG_volatile_type => is_register_type(&tipe, unit),
        gimli::DW_TAG_typedef => is_register_type(&tipe, unit),
        gimli::DW_TAG_const_type => is_register_type(&tipe, unit),
        gimli::DW_TAG_structure_type => Ok(false),
        gimli::DW_TAG_union_type => {
            // A union fits in a register if all of its variants do
            let mut tree = unit.entries_tree(Some(tipe.offset()))?;
            let root = tree.root()?;
            let mut children = root.children();

            while let Some(child) = children.next()? {
                if !is_register_type(child.entry(), unit)? {
                    return Ok(false);
                }
            }

            Ok(true)
        }
        x => Err(anyhow!("entry has unknown type type: {x:?}")),
    }
}

fn dump_file(
    path: &str,
    object: &object::File,
    endian: gimli::RunTimeEndian,
    output: Output,
) -> Result<()> {
    let arena_data = Arena::new();
    let arena_relocations = Arena::new();

    // Load a section and return as `Cow<[u8]>`.
    let load_section = |id: gimli::SectionId| -> Result<_> {
        reloc::load_file_section(id, object, endian, false, &arena_data, &arena_relocations)
    };

    // Load all of the sections.
    let dwarf = gimli::Dwarf::load(&load_section)?;

    // Iterate over the compilation units.
    let mut iter = dwarf.units();
    while let Some(header) = iter.next()? {
        let unit = dwarf.unit(header)?;
        let mut tree = unit.entries_tree(None)?;
        let root = tree.root()?;
        let mut children = root.children();

        while let Some(child) = children.next()? {
            let funcentry = child.entry();
            let funcoffset = entry_to_die_offset(funcentry, &unit).unwrap();

            if funcentry.tag() != gimli::DW_TAG_subprogram
                || !is_concrete_function(funcentry)
                    .with_context(|| format!("{path}+{funcoffset:#x} is concrete?"))?
                || !die_has_c_source(funcentry, &dwarf, &unit)
                    .with_context(|| format!("{path}+{funcoffset:#x} has C source?"))?
            {
                continue;
            }

            let name = match funcentry
                .attr_value(gimli::DW_AT_name)?
                .and_then(|x| attr_to_string(x, &dwarf, &unit))
            {
                Some(x) => x,
                None => continue, // Apparently this may happen in C++, on ARM, sometimes?
            };

            let range = match CodeRange::from_function_die(funcentry)
                .with_context(|| format!("looking up {path}+{funcoffset:#x} extent"))?
            {
                Some(x) => x,
                // Apparently this may happen on ARM, where on amd64 we'd
                // generate an empty function, ARM elides the call (but
                // not the DWARF?)
                None => continue,
            };

            let frame_base = match funcentry.attr_value(gimli::DW_AT_frame_base)? {
                Some(x) => x,
                None => {
                    eprintln!("{path}+{funcoffset:#x}: WARNING: {name}() has no frame base");
                    continue;
                }
            };

            let base_offset = match BaseOffset::from_frame_base(
                frame_base, &range, object, &dwarf, &unit,
            ) {
                Err(x) => {
                    eprintln!("{path}+{funcoffset:#x} {name}(): reading frame base: {x:?}");
                    continue;
                }
                Ok(Some(x)) => x,
                Ok(None) => {
                    eprintln!(
                        "{path}+{funcoffset:#x}: WARNING: {name}() has no recognized base-pointer"
                    );
                    continue;
                }
            };

            let mut nparams = 0;
            let mut found = false;
            let mut children = child.children();
            while let Some(child) = children.next()? {
                let childentry = child.entry();
                let childoffset = entry_to_die_offset(childentry, &unit).unwrap();

                match childentry.tag() {
                    gimli::DW_TAG_formal_parameter => {
                        if is_register_type(childentry, &unit).with_context(|| {
                            format!("{path}+{childoffset:#x}: checking parameter type")
                        })? {
                            nparams += 1;
                        } else {
                            found = true; // Really, we've found it to be invalid
                            continue;
                        }
                    }
                    gimli::DW_TAG_variable => (),
                    _ => continue,
                }

                match childentry
                    .attr_value(gimli::DW_AT_name)?
                    .and_then(|x| attr_to_string(x, &dwarf, &unit))
                {
                    Some(x) if x == "__illumos_saved_args_v1__" => x,
                    _ => continue, // Nameless variables exist, but we needn't worry
                };

                // Our symbol is decidedly unreal
                if childentry.attr_value(gimli::DW_AT_artificial)?.is_none() {
                    eprintln!("{path}+{childoffset:#x}: WARNING: {name}() __illumos_saved_args_v1__ is not artificial");
                }

                if let Some(e) = childentry
                    .attr_value(gimli::DW_AT_location)?
                    .unwrap()
                    .exprloc_value()
                {
                    let mut ops = e.operations(unit.encoding());

                    if let Some(op) = ops.next()? {
                        match op {
                            gimli::read::Operation::FrameOffset { offset: off } => {
                                let locstr = base_offset
                                    .valid
                                    .iter()
                                    .map(|x| format!("[+{:#x},+{:#x})", x.start, x.end))
                                    .join(", ");

                                let inv_offset = base_offset.invert(&range);

                                let opstr = inv_offset
                                    .iter()
                                    .map(|x| format!("[+{:#x},+{:#x})", x.start, x.end))
                                    .join(", ");

                                let goodperc = base_offset
                                    .valid
                                    .iter()
                                    .fold(0, |acc, x| acc + (x.end - x.start).max(1))
                                    as f64
                                    / (range.end - range.start) as f64
                                    * 100.0;

                                let badperc = 100.0 - goodperc;

                                match output {
                                    Output::Json => println!(
                                        "{}",
                                        serde_json::to_string(&serde_json::json!({"name": name,
                                                                                  "nparams": nparams,
                                                                                  "offset": base_offset.offset + off,
                                                                                  "valid": base_offset.valid,
                                                                                  "invalid": inv_offset,
                                        }))?
                                    ),
                                    Output::Text => println!(
                                        "{path}+{funcoffset:#x} {name}() has {nparams} \
                                         saved arguments at frame offset {} \
                                         valid in {locstr} ({goodperc:2.2}%) \
                                         invalid in {opstr} ({badperc:2.2}%)",
                                        base_offset.offset + off
                                    ),
                                }
                            }
                            x => {
                                eprintln!("{path}+{childoffset:#x}: WARNING: {name}() __illumos_saved_args_v1__ has unexpected location expression: {x:?}");
                                continue;
                            }
                        }
                    }

                    if ops.count()? != 0 {
                        eprintln!("{path}+{childoffset:#x}: WARNING: {name}() __illumos_saved_args_v1__ has extra location operations");
                        continue;
                    }

                    found = true;
                }
            }

            if nparams != 0 && !found {
                eprintln!(
                    "{path}+{funcoffset:#x}: WARNING: {name}(): {nparams} parameters but no saved args"
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum Output {
    Text,
    Json,
}

fn main() -> Result<()> {
    let mut opts = getopts::Options::new();
    opts.optflag("j", "json", "json output");
    let matches = opts.parse(env::args().skip(1))?;
    let output = if matches.opt_present("j") {
        Output::Json
    } else {
        Output::Text
    };

    for path in matches.free {
        let file = fs::File::open(&path).with_context(|| format!("Opening {path}"))?;
        let mmap = unsafe { memmap2::Mmap::map(&file).with_context(|| format!("Mapping {path}"))? };
        let object = object::File::parse(&*mmap).with_context(|| format!("Parsing {path}"))?;
        let endian = if object.is_little_endian() {
            gimli::RunTimeEndian::Little
        } else {
            gimli::RunTimeEndian::Big
        };

        if object.section_by_name(".SUNW_ctf").is_none() {
            continue; // No CTF, no saved arguments
        }

        match object.kind() {
            ObjectKind::Executable | ObjectKind::Dynamic | ObjectKind::Relocatable => {
                if let Err(x) = dump_file(&path, &object, endian, output) {
                    eprintln!("{path}: WARNING: failed to examine: {x:?}");
                }
            }
            _ => eprintln!(
                "{path}: WARNING: only executable, relocatable and dynamic objects and supported"
            ),
        }
    }

    Ok(())
}
