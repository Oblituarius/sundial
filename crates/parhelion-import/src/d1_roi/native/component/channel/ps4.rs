//! PS4 Rise of Iron channel banks: the schema block sits 0x20 before Xbox One's and receiver
//! declarations are 32 bytes. Retains inherited inputs, unbound zero values, bounded local
//! constants, local channel procedures with an optional kind-2 callback, named variables and
//! optic-fed receivers. Xbox One banks convert through the parent module.
use super::*;

/// Rise of Iron's callback class for kind 2 and Shadowkeep's. Native callback classes are
/// `808097B0` less the kind.
const SOURCE_KIND_2_CALLBACK: u32 = 0x8080_072D;
const NATIVE_KIND_2_CALLBACK: u32 = 0x8080_97AE;

/// A local channel computed by a procedure over input channels of its bank, optionally
/// through a callback. Rise of Iron and Shadowkeep run the same channel bytecode: every
/// opcode in the 5138 programmed D1 rows of the PS4 dump also occurs in the 9943 programmed
/// native rows, byte-identical programs such as `3C 00 22 AA 3C 00 22 AA 0C …` occur in both,
/// and native rows hold multi-input programs, spline programs over ten constants and kind-2
/// callbacks in the same fields. The program, constants and input ordinals carry over as they
/// are.
struct Procedure {
    code: Vec<u8>,
    constants: Vec<u8>,
    inputs: Vec<u32>,
    value_type: u16,
    /// The kind-2 callback object: kind 2, an unused `FFFFFFFF` word and two floats, the same
    /// 16 bytes in both versions. Shadowkeep's rocket launcher bank `8152A4F7` holds callback
    /// rows of this shape.
    callback: Option<[u8; 16]>,
}

struct Bank {
    names: Vec<u32>,
    /// Parent lookup namespace, parent channel name and local ordinal, sorted by namespace
    /// then name as the native lookup is.
    inherited: Vec<(u32, u32, usize)>,
    constants: BTreeMap<usize, [u8; 16]>,
    procedures: BTreeMap<usize, Procedure>,
    /// Local channels without a program, which other systems write by name. Both versions
    /// hold rows of this shape with a dependency mask: 13k in the PS4 dump, 29k in the native
    /// banks. Their value starts at zero.
    variables: BTreeSet<usize>,
    /// The value type of each channel without a program, zero for programmed channels.
    value_types: Vec<u16>,
    dependencies: Vec<Vec<u8>>,
    /// Channels another component of the entity writes through a numeric provider, which
    /// hold the first ordinals.
    receivers: Vec<Receiver>,
}

/// A channel written by a numeric provider: the lookup name the provider publishes under and
/// the source declaration its entity connection targets.
#[derive(Clone)]
pub(in super::super) struct Receiver {
    namespace: u32,
    property: u32,
    pub declaration: usize,
}

impl Bank {
    /// Local channels, constants, procedures and variables, by name as the native local lookup
    /// sorts them.
    fn locals(&self) -> Vec<(u32, u32, usize)> {
        let mut locals = self
            .constants
            .keys()
            .chain(self.procedures.keys())
            .chain(&self.variables)
            .map(|&i| (0x811C9DC5, self.names[i], i))
            .collect::<Vec<_>>();
        locals.sort_unstable();
        locals
    }
}

/// Constants a channel opcode consumes from its operand onward, after Charm's D1 TFX table
/// (`TfxBytecode_D1`), which Shadowkeep's (`TfxBytecode_BL`) matches through 0x40: push, lerp,
/// saturated lerp, 4- and 8-point splines, chained spline, and 4- and 8-point gradients.
/// Charm's interpreter and alkahest's sequencer VM read the same spans.
fn constant_span(op: u8) -> Option<usize> {
    Some(match op {
        0x34 => 1,
        0x35 | 0x36 => 2,
        0x37 => 5,
        0x38 | 0x39 => 10,
        0x3A => 6,
        0x3B => 11,
        _ => return None,
    })
}

/// Check a channel program: Charm's D1 arithmetic opcodes, permutes, constant reads within
/// its table, input reads within its inputs, and a single final return of output 0. In
/// sequencer bytecode Charm reads `3C` (extern float) as an input read and `3E` as the output
/// pop, each with one operand byte where material expressions give them two.
fn validate_program(code: &[u8], constants: usize, inputs: usize) -> Result<()> {
    let mut at = 0;
    while at < code.len() {
        let op = code[at];
        let operand = match op {
            0x22 | 0x34..=0x3C | 0x3E => {
                Some(*code.get(at + 1).context("Truncated channel program")?)
            }
            0x01..=0x21 | 0x23 | 0x25..=0x2E => None,
            _ => anyhow::bail!("Unsupported source channel opcode {op:02X}"),
        };
        if let (Some(span), Some(first)) = (constant_span(op), operand) {
            ensure!(
                usize::from(first) + span <= constants,
                "Source channel constant exceeds its table"
            );
        }
        if op == 0x3C {
            ensure!(
                usize::from(operand.unwrap_or(0)) < inputs,
                "Source channel reads an undeclared input"
            );
        }
        at += 1 + usize::from(operand.is_some());
        if op == 0x3E {
            ensure!(
                operand == Some(0) && at == code.len(),
                "Source channel return differs"
            );
            return Ok(());
        }
    }
    anyhow::bail!("Source channel program does not return")
}

/// A local channel procedure row. The caller has excluded empty and constant rows.
fn procedure(source: &Payload, row: usize, code: Vec<u8>, channels: usize) -> Result<Procedure> {
    let inputs = source
        .array(row + 56, 4, Some(0x80800007))?
        .into_iter()
        .map(|at| source.u32(at))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        inputs
            .iter()
            .all(|&input| usize::try_from(input).is_ok_and(|i| i < channels)),
        "Source channel input is not a channel of its bank"
    );
    let constants = source
        .array(row + 24, 16, Some(0x80800090))?
        .into_iter()
        .flat_map(|at| source.0[at..at + 16].iter().copied())
        .collect::<Vec<_>>();
    ensure!(
        constants
            .chunks_exact(4)
            .all(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()).is_finite()),
        "Nonfinite source channel constant"
    );
    validate_program(&code, constants.len() / 16, inputs.len())?;
    let value_type = source.u16(row + 74)?;
    ensure!(
        source.u64(row + 40)? == inputs.len() as u64
            && source.u64(row + 48)? == 1
            && source.u16(row + 72)? == u16::MAX
            && value_type <= 5
            && source.bytes::<2>(row + 104)? == [255, 254],
        "Source channel procedure layout differs"
    );
    let callback = if source.u64(row + 96)? == 0 {
        None
    } else {
        let object = source.pointer(row + 96)?;
        ensure!(
            object >= 4 && source.u32(object - 4)? == SOURCE_KIND_2_CALLBACK,
            "Source channel callback kind requires translation"
        );
        ensure!(
            source.u32(object)? == 2
                && source.u32(object + 4)? == u32::MAX
                && [8, 12]
                    .iter()
                    .all(|at| source.f32(object + at).is_ok_and(f32::is_finite)),
            "Source kind-2 callback layout differs"
        );
        Some(source.bytes::<16>(object)?)
    };
    Ok(Procedure {
        code,
        constants,
        inputs,
        value_type,
        callback,
    })
}

impl Bank {
    /// `shift` is how far the PS4 schema block sits before Xbox One's (0x20), else 0.
    fn read(source: &Payload, shift: usize) -> Result<Self> {
        let schema = source.pointer(24)? - shift;
        let rows = source.array(schema + 0xD8, 112, Some(0x80800883))?;
        ensure!(
            !rows.is_empty() && rows.len() <= 64,
            "Source channel count differs"
        );
        let names = rows
            .iter()
            .map(|at| source.u32(*at))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            names.iter().copied().collect::<BTreeSet<_>>().len() == names.len(),
            "Duplicate source channel name"
        );
        let mut inherited = Vec::new();
        for (slot, at) in source
            .array(schema + 0x108, 12, None)?
            .into_iter()
            .enumerate()
        {
            let ordinal = usize::try_from(source.u32(at + 8)?)?;
            let row = *rows.get(ordinal).context("Source inherited declaration")?;
            // An empty namespace takes the parent channel of the local name. A named namespace
            // takes a parent channel under its own name, as Shadowkeep's rocket launcher bank
            // 8152A4F7 does for the same namespace 5F6317D5.
            let namespace = source.u32(at)?;
            let parent = source.u32(at + 4)?;
            ensure!(
                (if namespace == 0x811C9DC5 {
                    parent == names[ordinal]
                } else {
                    ![0, u32::MAX, 0x811C9DC5].contains(&parent)
                }) && source.u16(row + 72)? as usize == slot,
                "Source inherited channel lookup differs"
            );
            inherited.push((namespace, parent, ordinal));
        }
        ensure!(
            inherited
                .windows(2)
                .all(|pair| (pair[0].0, pair[0].1) < (pair[1].0, pair[1].1)),
            "Source inherited names are unsorted"
        );
        let inherited_ordinals = inherited
            .iter()
            .map(|(_, _, ordinal)| *ordinal)
            .collect::<BTreeSet<_>>();
        let mut constants = BTreeMap::new();
        let mut procedures = BTreeMap::new();
        let mut variables = BTreeSet::new();
        let mut value_types = vec![0; rows.len()];
        let mut dependencies = Vec::new();
        for (ordinal, &row) in rows.iter().enumerate() {
            let code = source
                .array(row + 8, 1, Some(0x80800009))?
                .iter()
                .map(|at| source.u8(*at))
                .collect::<Result<Vec<_>>>()?;
            let values = source.array(row + 24, 16, Some(0x80800090))?;
            // D1 and native 8152B5B7 both use this zero-input vec4 return.
            let constant = code == [0x34, 0, 0x3E, 0]
                && values.len() == 1
                && source.u64(row + 40)? == 0
                && source.u64(row + 48)? == 1
                && source.u64(row + 56)? == 0
                && source.u64(row + 64)? == 0
                && source.u16(row + 72)? == u16::MAX
                && source.u16(row + 74)? == 1
                && source.u64(row + 96)? == 0;
            let programmed = !code.is_empty();
            if programmed && !constant {
                procedures.insert(ordinal, procedure(source, row, code, rows.len())?);
            } else if !programmed {
                ensure!(
                    source.bytes::<2>(row + 104)? == [255, 254] && source.u64(row + 96)? == 0,
                    "Source channel has unsupported callbacks or interpolation"
                );
                let inherited = inherited_ordinals.contains(&ordinal);
                let value_type = source.u16(row + 74)?;
                ensure!(
                    source.0[row + 16..row + 72].iter().all(|v| *v == 0)
                        && value_type <= 5
                        && (!inherited || value_type == 0),
                    "Source channel has unsupported defaults or forwarding"
                );
                ensure!(
                    inherited == (source.u16(row + 72)? != u16::MAX),
                    "Source channel inherited slot lacks a name"
                );
                // A local index names a variable; the local lookup check below confirms it.
                if source.u16(row + 106)? != u16::MAX {
                    ensure!(!inherited, "Inherited source channel is also local");
                    variables.insert(ordinal);
                }
                value_types[ordinal] = value_type;
            } else {
                ensure!(
                    source.bytes::<2>(row + 104)? == [255, 254],
                    "Source channel has unsupported callbacks or interpolation"
                );
                let value = source.bytes::<16>(values[0])?;
                ensure!(
                    value
                        .chunks_exact(4)
                        .all(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()).is_finite()),
                    "Nonfinite source channel constant"
                );
                constants.insert(ordinal, value);
            }
            let words = source.array(row + 80, 4, Some(0x8080000B))?;
            ensure!(
                words.is_empty() || words.len() == rows.len().div_ceil(32),
                "Source channel dependency width differs"
            );
            // Masks mark the channels that read a changed input. Neither version gives a
            // programmed row one, and ordinals carry over, so input masks carry over as they are.
            ensure!(
                !programmed || words.is_empty(),
                "Programmed source channel has unsupported dependencies"
            );
            let bytes = words
                .iter()
                .flat_map(|at| source.0[*at..*at + 4].iter().copied())
                .collect::<Vec<_>>();
            if let Some(&last) = words.last() {
                let tail = rows.len() % 32;
                ensure!(
                    tail == 0 || source.u32(last)? >> tail == 0,
                    "Source dependency exceeds channel declarations"
                );
            }
            dependencies.push(bytes);
        }
        let local = source.array(schema + 0xF8, 12, None)?;
        ensure!(
            local.len() == constants.len() + procedures.len() + variables.len(),
            "Source channel local names differ"
        );
        let mut local_ordinals = BTreeSet::new();
        let mut local_indices = BTreeSet::new();
        let count = local.len();
        for at in local {
            let ordinal = usize::try_from(source.u32(at + 8)?)?;
            let row = *rows.get(ordinal).context("Source local channel ordinal")?;
            // Each local row names a distinct place among the locals. Native rows name their
            // place in the sorted lookup, which the conversion writes afresh.
            let index = usize::from(source.u16(row + 106)?);
            ensure!(
                (constants.contains_key(&ordinal)
                    || procedures.contains_key(&ordinal)
                    || variables.contains(&ordinal))
                    && index < count
                    && local_indices.insert(index)
                    && local_ordinals.insert(ordinal)
                    && source.u32(at)? == 0x811C9DC5
                    && source.u32(at + 4)? == names[ordinal],
                "Source local channel lookup differs"
            );
        }
        let instance = source.pointer(16)?;
        ensure!(
            source.u64(instance + 0x70)? == 0
                && source.u64(instance + 0x80)? == 0
                && source.u64(schema + 0xE8)? == 0,
            "Source channel has unsupported receivers"
        );
        let receivers = receivers(source, schema, instance, &names, &inherited_ordinals)?;
        let values = source.array(instance + 0x50, 16, Some(0x80800090))?;
        ensure!(
            values.len() == rows.len()
                && values
                    .iter()
                    .all(|at| source.0[*at..*at + 16].iter().all(|v| *v == 0)),
            "Source cached channel initialization differs"
        );
        Ok(Self {
            names,
            inherited,
            constants,
            procedures,
            variables,
            value_types,
            dependencies,
            receivers,
        })
    }
}

/// Source receivers in ordinal order. Every one of the 1946 receiver banks in the PS4 dump
/// gives its receivers the first ordinals, one declaration and one state each, in ordinal
/// order, as native `tiger::channel::inputs` also requires.
fn receivers(
    source: &Payload,
    schema: usize,
    instance: usize,
    names: &[u32],
    inherited: &BTreeSet<usize>,
) -> Result<Vec<Receiver>> {
    let lookup = source.array(schema + 0x118, 12, None)?;
    let declarations = source.array(schema + 0xC8, 32, Some(0x80800668))?;
    ensure!(
        declarations.len() == lookup.len()
            && source.array(instance + 0x30, 80, None)?.len() == lookup.len(),
        "Source channel receiver count differs"
    );
    let owner = source.u32(instance)?;
    let mut receivers = vec![None; lookup.len()];
    for at in lookup {
        let ordinal = usize::try_from(source.u32(at + 8)?)?;
        let slot = receivers
            .get_mut(ordinal)
            .context("Source channel receiver is not among the first ordinals")?;
        ensure!(
            slot.is_none() && ordinal < names.len() && !inherited.contains(&ordinal),
            "Source channel receiver lookup differs"
        );
        let declaration = declarations[ordinal];
        // Owner, state offset, state class 80800895 and instance class 80800539, which
        // native declarations hold as 808097A7 and 80809AE2.
        ensure!(
            source.u32(declaration)? == owner
                && source.u32(declaration + 4)? == 0
                && source.u32(declaration + 16)? == 0x8080_0895
                && source.u64(declaration + 20)? == 0
                && source.u32(declaration + 28)? == 0x8080_0539,
            "Source channel receiver declaration differs"
        );
        *slot = Some(Receiver {
            namespace: source.u32(at)?,
            property: source.u32(at + 4)?,
            declaration,
        });
    }
    Ok(receivers.into_iter().flatten().collect())
}

fn lookup(rows: &[(u32, u32, usize)]) -> Result<Vec<u8>> {
    rows.iter()
        .map(|&(namespace, name, ordinal)| {
            Ok([
                namespace.to_le_bytes(),
                name.to_le_bytes(),
                u32::try_from(ordinal)?.to_le_bytes(),
            ]
            .concat())
        })
        .collect::<Result<Vec<_>>>()
        .map(|rows| rows.concat())
}

/// The declaration rows of every channel, before their arrays.
fn declaration_rows(
    bank: &Bank,
    locals: &[(u32, u32, usize)],
    constant_template: Option<[u8; 112]>,
) -> Result<Vec<u8>> {
    let mut declarations = Vec::new();
    // A local row names its place in the name-sorted local lookup, as `8152A4F7` shows.
    let position = |ordinal: usize| -> Result<u16> {
        let index = locals
            .iter()
            .position(|(_, _, i)| *i == ordinal)
            .context("Local channel lookup position")?;
        Ok(u16::try_from(index)?)
    };
    for (ordinal, name) in bank.names.iter().enumerate() {
        let mut row = if bank.constants.contains_key(&ordinal) {
            let mut row = constant_template.context("Native local channel template")?;
            row[8..40].fill(0);
            put(&mut row, 106, &position(ordinal)?.to_le_bytes())?;
            row
        } else if let Some(procedure) = bank.procedures.get(&ordinal) {
            let mut row = [0; 112];
            put(&mut row, 40, &(procedure.inputs.len() as u64).to_le_bytes())?;
            put(&mut row, 48, &1u64.to_le_bytes())?;
            put(&mut row, 74, &procedure.value_type.to_le_bytes())?;
            row[104..106].copy_from_slice(&[255, 254]);
            put(&mut row, 106, &position(ordinal)?.to_le_bytes())?;
            row
        } else {
            let mut row = [0; 112];
            put(&mut row, 74, &bank.value_types[ordinal].to_le_bytes())?;
            row[104..108].copy_from_slice(&[255, 254, 255, 255]);
            if bank.variables.contains(&ordinal) {
                put(&mut row, 106, &position(ordinal)?.to_le_bytes())?;
            }
            row
        };
        put(&mut row, 0, &name.to_le_bytes())?;
        let slot = bank
            .inherited
            .iter()
            .position(|(_, _, i)| *i == ordinal)
            .map(u16::try_from)
            .transpose()?
            .unwrap_or(u16::MAX);
        put(&mut row, 72, &slot.to_le_bytes())?;
        declarations.extend(row);
    }
    Ok(declarations)
}

/// Each channel's dependency mask, constant, procedure, inputs and callback object.
fn write_programs(p: &mut Payload, schema: usize, bank: &Bank) -> Result<()> {
    for (ordinal, row) in p
        .array(schema + 0xD8, 112, Some(0x808097A1))?
        .into_iter()
        .enumerate()
    {
        array(
            &mut p.0,
            row + 80,
            0x8080000B,
            &bank.dependencies[ordinal],
            4,
        )?;
        if let Some(value) = bank.constants.get(&ordinal) {
            array(&mut p.0, row + 8, 0x80800009, &[0x34, 0, 0x3E, 0], 1)?;
            array(&mut p.0, row + 24, 0x80800090, value, 16)?;
        }
        if let Some(procedure) = bank.procedures.get(&ordinal) {
            array(&mut p.0, row + 8, 0x80800009, &procedure.code, 1)?;
            if !procedure.constants.is_empty() {
                array(&mut p.0, row + 24, 0x80800090, &procedure.constants, 16)?;
            }
            let inputs = procedure
                .inputs
                .iter()
                .flat_map(|input| input.to_le_bytes())
                .collect::<Vec<_>>();
            array(&mut p.0, row + 56, 0x80800007, &inputs, 4)?;
            if let Some(callback) = procedure.callback {
                // The callback object follows inline, eight-byte aligned behind its class
                // word.
                let object = (p.0.len() + 4).next_multiple_of(8);
                p.0.resize(object - 4, 0);
                p.0.extend(NATIVE_KIND_2_CALLBACK.to_le_bytes());
                p.0.extend(callback);
                put(
                    &mut p.0,
                    row + 96,
                    &(object as i64 - (row + 96) as i64).to_le_bytes(),
                )?;
            }
        }
    }
    Ok(())
}

/// Set each named allocation row from its template count to the converted count.
fn resize_allocation(allocation: &mut Payload, rows: &[(u32, u32, usize, usize)]) -> Result<()> {
    for &(name, class, before, after) in rows {
        let matches = allocation
            .array(0x20, 40, Some(0x80808852))?
            .into_iter()
            .filter(|r| allocation.u32(*r).ok() == Some(name))
            .collect::<Vec<_>>();
        if before == 0 && after == 0 && matches.is_empty() {
            continue;
        }
        ensure!(matches.len() == 1, "Native channel allocation is ambiguous");
        let row = matches[0];
        ensure!(
            allocation.u32(row + 16)? == class && allocation.u32(row + 20)? as usize == before,
            "Native channel capacity differs"
        );
        put(
            &mut allocation.0,
            row + 20,
            &u32::try_from(after)?.to_le_bytes(),
        )?;
    }
    Ok(())
}

/// One state and one reciprocal declaration per receiver, linked as
/// `tiger::channel::inputs::extend` links them, and the receiver lookup.
fn write_receivers(
    p: &mut Payload,
    instance: usize,
    schema: usize,
    receivers: &[Receiver],
    (state, declaration): ([u8; 80], [u8; 32]),
) -> Result<()> {
    array(
        &mut p.0,
        instance + 0x30,
        0x808097A7,
        &state.repeat(receivers.len()),
        80,
    )?;
    array(
        &mut p.0,
        schema + 0xC8,
        0x808097A8,
        &declaration.repeat(receivers.len()),
        32,
    )?;
    let states = p.array(instance + 0x30, 80, Some(0x808097A7))?;
    let declarations = p.array(schema + 0xC8, 32, Some(0x808097A8))?;
    for (&state, &declaration) in states.iter().zip(&declarations) {
        put(&mut p.0, state + 8, &(declaration as u64).to_le_bytes())?;
        put(
            &mut p.0,
            state + 16,
            &(instance as i64 - state as i64 - 16).to_le_bytes(),
        )?;
        put(&mut p.0, declaration + 8, &(state as u64).to_le_bytes())?;
    }
    let mut names = receivers
        .iter()
        .enumerate()
        .map(|(ordinal, r)| (r.namespace, r.property, ordinal))
        .collect::<Vec<_>>();
    names.sort_unstable();
    array(&mut p.0, schema + 0x118, 0x808097A0, &lookup(&names)?, 12)?;
    Ok(())
}

/// The declared channel names of a source bank in ordinal order, which a material's object
/// inputs index.
pub(in super::super) fn channel_names(source: &Payload, shift: usize) -> Result<Vec<u32>> {
    Ok(Bank::read(source, shift)?.names)
}

impl Assembly<'_> {
    /// The local constant row of `8152B5B7`, whichever bank template holds the rest.
    fn ps4_constant_row(&mut self) -> Result<[u8; 112]> {
        let carrier = self.native.tag(0x8152B5B7, Some(0x80809C36))?;
        let carrier_schema = carrier.pointer(24)?;
        let candidates = carrier
            .array(carrier_schema + 0xD8, 112, Some(0x808097A1))?
            .into_iter()
            .filter(|at| carrier.u64(*at + 8).ok() == Some(4))
            .collect::<Vec<_>>();
        ensure!(
            candidates.len() == 1,
            "Native constant channel template is ambiguous"
        );
        let row = candidates[0];
        let code = carrier
            .array(row + 8, 1, Some(0x80800009))?
            .iter()
            .map(|at| carrier.u8(*at))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            code == [0x34, 0, 0x3E, 0]
                && carrier.u64(row + 40)? == 0
                && carrier.u64(row + 48)? == 1
                && carrier.u64(row + 56)? == 0
                && carrier.u16(row + 72)? == u16::MAX
                && carrier.u16(row + 74)? == 0
                && carrier.u64(row + 80)? == 0
                && carrier.u64(row + 96)? == 0
                && carrier.bytes::<4>(row + 104)? == [255, 254, 0, 0],
            "Native constant channel contract differs"
        );
        carrier.bytes::<112>(row)
    }

    pub(in super::super) fn channel_bank_ps4(
        &mut self,
        source: &Payload,
        symbol: &str,
        index: usize,
        connect_receivers: bool,
    ) -> Result<(Component, Vec<u32>, Vec<Receiver>)> {
        let bank = Bank::read(source, self.source.field(0, 0x20))?;
        let locals = bank.locals();
        // Receivers the entity cannot feed stay plain channels at zero.
        let receivers = if connect_receivers {
            bank.receivers.clone()
        } else {
            Vec::new()
        };
        // Stock optic bank `81532E86` holds one receiver, its state and its allocation, under
        // the same name `4F437B90` that D1 sight banks receive from their optic.
        let (template, entity) = if !receivers.is_empty() {
            (0x81532E86, 0x8161ECA5)
        } else if locals.is_empty() {
            (0x80F56FA1, 0x80BC358D)
        } else {
            (0x8152B5B7, 0x81523111)
        };
        let mut component = self.component(template, entity, symbol, index)?;
        let original = &component.template;
        let instance = original.pointer(16)?;
        let schema = original.pointer(24)?;
        let original_rows = original.array(schema + 0xD8, 112, Some(0x808097A1))?;
        let pending = original.array(instance + 0x40, 4, Some(0x8080000B))?;
        ensure!(
            pending.len() == original_rows.len().div_ceil(32)
                && pending.iter().all(|at| original.u32(*at).ok() == Some(0)),
            "Native pending channel initialization differs"
        );
        ensure!(
            original.u32(schema + 0xC0)? == u32::MAX && original.u32(schema + 0xC4)? == u32::MAX,
            "Native bank template names special channels"
        );
        let constant_template = if bank.constants.is_empty() {
            None
        } else {
            Some(self.ps4_constant_row()?)
        };
        let original_states = original.u64(instance + 0x30)? as usize;
        let receiver_templates = if receivers.is_empty() {
            None
        } else {
            let payload = &component.payload;
            let state = *payload
                .array(instance + 0x30, 80, Some(0x808097A7))?
                .first()
                .context("Native receiver state template")?;
            let declaration = *payload
                .array(schema + 0xC8, 32, Some(0x808097A8))?
                .first()
                .context("Native receiver declaration template")?;
            Some((
                payload.bytes::<80>(state)?,
                payload.bytes::<32>(declaration)?,
            ))
        };
        let p = &mut component.payload;
        let declarations = declaration_rows(&bank, &locals, constant_template)?;
        array(&mut p.0, schema + 0xD8, 0x808097A1, &declarations, 112)?;
        write_programs(p, schema, &bank)?;
        array(
            &mut p.0,
            instance + 0x40,
            0x8080000B,
            &vec![0; bank.names.len().div_ceil(32) * 4],
            4,
        )?;
        array(
            &mut p.0,
            instance + 0x50,
            0x80800090,
            &vec![0; bank.names.len() * 16],
            16,
        )?;
        array(
            &mut p.0,
            instance + 0x90,
            0x8080979C,
            &vec![255; bank.inherited.len() * 16],
            16,
        )?;
        array(
            &mut p.0,
            instance + 0x80,
            0x8080979D,
            &vec![255; locals.len() * 8],
            8,
        )?;
        array(
            &mut p.0,
            schema + 0x108,
            0x808097A0,
            &lookup(&bank.inherited)?,
            12,
        )?;
        array(&mut p.0, schema + 0xF8, 0x808097A0, &lookup(&locals)?, 12)?;
        for (at, count) in [
            (0x134, bank.names.len()),
            (0x13C, locals.len()),
            (0x140, bank.inherited.len()),
        ] {
            put(&mut p.0, schema + at, &u32::try_from(count)?.to_le_bytes())?;
        }
        // The inherited range is the first and last slot, or FFFF twice without inherited
        // channels, as in the 164 native banks without them (`80FDB503`, `81585242`).
        let (first, last) = match bank.inherited.len() {
            0 => (u16::MAX, u16::MAX),
            count => (0, u16::try_from(count - 1)?),
        };
        put(&mut p.0, schema + 0x128, &first.to_le_bytes())?;
        put(&mut p.0, schema + 0x12A, &last.to_le_bytes())?;
        put(
            &mut p.0,
            schema + 0x130,
            &u32::try_from(bank.names.len().div_ceil(32))?.to_le_bytes(),
        )?;
        if let Some(templates) = receiver_templates {
            write_receivers(p, instance, schema, &receivers, templates)?;
        }
        let tag = p.u32(0x44)?;
        let mut allocation = (*self.native.tag(tag, None)?).clone();
        resize_allocation(
            &mut allocation,
            &[
                (
                    0x4A423762,
                    0x8080000B,
                    pending.len(),
                    bank.names.len().div_ceil(32),
                ),
                (
                    0xFC2F3D6F,
                    0x80800090,
                    original_rows.len(),
                    bank.names.len(),
                ),
                (
                    0x3A55A801,
                    0x8080979C,
                    original.u64(instance + 0x90)? as usize,
                    bank.inherited.len(),
                ),
                (
                    0xBAFA25AF,
                    0x8080979D,
                    original.u64(instance + 0x80)? as usize,
                    locals.len(),
                ),
                (0xA8333EC9, 0x808097A7, original_states, receivers.len()),
            ],
        )?;
        let allocation_symbol = format!("{symbol}-allocation");
        self.node(&allocation_symbol, tag, &allocation.0, None, &[])?;
        component.patch(0x44, &allocation_symbol)?;
        component.seal()?;
        self.evidence.push(json!({"source_channel_bank":source.u32(source.pointer(16)?)?,
            "declarations":bank.names.len(),"inherited":bank.inherited.len(),"local_constants":bank.constants.len(),
            "local_procedures":bank.procedures.len(),
            "local_callbacks":bank.procedures.values().filter(|p| p.callback.is_some()).count(),
            "local_variables":bank.variables.len(),"receivers":receivers.len(),
            "unfed_receivers":bank.receivers.len()-receivers.len(),
            "unbound_zero":bank.names.len()-bank.inherited.len()-locals.len()}));
        if receivers.len() < bank.receivers.len() {
            self.approximations
                .insert("Source channels written by untranslated components hold zero".to_owned());
        }
        Ok((component, bank.names, receivers))
    }
}
