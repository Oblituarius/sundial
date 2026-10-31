use super::super::expression;
use super::*;
use crate::tiger::shader::{
    identity::Stage,
    program::{Program, Template},
};

/// Stock sight materials `80BA7DA2` and `80EF364B`, whose reticle quads match the D1
/// quad's dimensions, subtract this from the quad's z before the reticle projection.
const NATIVE_SIGHT_DEPTH: f32 = 2.5;

pub(super) struct Equipment {
    pub plates: [String; 3],
    /// The gearstack plate in the stock gear convention, for PS4 draws.
    pub gear_stack: String,
    pub dyes: BTreeMap<u8, u32>,
}

pub(super) struct Draw {
    pub stage: usize,
    pub dye: u8,
    pub simulated: bool,
    pub layout: u16,
}

/// The native state a D1 decal's blend state becomes, and whether it replaces target 0's color.
/// Material `+0x20` selects a blend state with its high bit. Both executables give states 22
/// and 23 the premultiplied blend `src + dst * src.a` that keeps the destination alpha, on
/// target 0 or targets 0 and 1, as native vehicle decal programs such as `815B9A49` expect.
/// D1 state 25 writes target 0's color without blending, where native state 25 blends three
/// targets, so it becomes native 22 with zero inverse coverage.
fn decal_state(state: u8) -> Result<(u8, bool)> {
    Ok(match state {
        0x96 | 0x97 => (state, false),
        0x99 => (0x96, true),
        _ => anyhow::bail!("Untranslated source decal blend state {state:02X}"),
    })
}

fn vectors(data: &Payload, field: usize) -> Result<Vec<u8>> {
    Ok(data
        .array(field, 16, Some(0x80800090))?
        .into_iter()
        .flat_map(|at| data.0[at..at + 16].iter().copied())
        .collect())
}

/// Stock techniques a PS4 draw borrows whole, by draw stage and native layout, since its
/// GCN programs have no translation. Each is the stock gear (sandbox or investment
/// package) technique drawn by the most models at that stage and layout among those with
/// no object inputs, which would index the stock entity's channels, and with no expression
/// reading an extern other than frame, view, `4E` or the gear extern. Of about 830,000 stock
/// draws only 111 bind a technique without a vertex program, and a PS4 item whose draws
/// all did froze or crashed the game on view. Stages 1, 7, 8 and 9 on layout 139 skip the
/// most drawn techniques, which read externs `2C` or `03`, which the imported item's draw
/// context also leaves unbound. Every texture a borrowed technique binds is a plain 2D
/// texture (type 32, subtype 1): the texture streaming callback (`0x1202830`) resolves only
/// those and passes any other kind's invalid result straight to a lookup, which crashed the
/// game on view of a weapon borrowing `815B37F1` and `80EF8E3D`, whose cubemap is
/// `80C1B780`. No gear technique for stages 7 and 9 on layout 123 qualifies, so those two
/// come from the globals package, drawn by 15 rigid models each.
const BORROWED_TECHNIQUES: [(usize, u16, u32); 30] = [
    (0, 30, 0x80EFAC1B),
    (0, 123, 0x80FC7DF6),
    (0, 138, 0x80C1D687),
    (0, 139, 0x80EF4DC0),
    (0, 143, 0x80BDFD59),
    (1, 30, 0x80C1901B),
    (1, 123, 0x815B3521),
    (1, 138, 0x80C1CAFF),
    (1, 139, 0x80C1CB18),
    (3, 30, 0x80EF938B),
    (3, 138, 0x80EF9389),
    (3, 139, 0x80EF3C7A),
    (3, 143, 0x80C68445),
    (7, 30, 0x80EF0F75),
    (7, 123, 0x80FDAE8B),
    (7, 138, 0x81531C4A),
    (7, 139, 0x815321AD),
    (7, 143, 0x80EFA482),
    (8, 139, 0x8161F38F),
    (9, 123, 0x80FDAE8C),
    (9, 139, 0x80C1AB2D),
    (10, 30, 0x80BF8671),
    (10, 138, 0x80C1CAFE),
    (10, 139, 0x80BF8667),
    (10, 143, 0x80BDFD5B),
    (12, 30, 0x80EFAD53),
    (12, 138, 0x80EFAD50),
    (12, 139, 0x80EFB192),
    (12, 143, 0x80EF4E84),
    (14, 139, 0x80EF5148),
];

/// The gear extern, a single vector that every stock gear pixel program on layouts 30 and
/// 138 reads. Only the gear feature renderer binds it, to zero, for each batch it draws
/// (`0x11B94E3` in the Shadowkeep executable). An imported item draws through the rigid
/// object renderer, which leaves the slot null, and the expression's read of it crashed the
/// game on view.
const GEAR_EXTERN: u8 = 0x56;

/// Shadowkeep expression operand sizes, as the native emission reader decodes them.
fn operand_bytes(op: u8) -> Result<usize> {
    Ok(match op {
        0x01..=0x21 | 0x23..=0x33 => 0,
        0x22 | 0x34..=0x3B | 0x42..=0x4E => 1,
        0x3C..=0x41 | 0x51..=0x53 => 2,
        _ => anyhow::bail!("Unsupported stock expression opcode {op:02X}"),
    })
}

/// The stock technique, with each stage expression's read of the gear extern replaced by
/// a push of the zero vector the gear renderer binds to it.
fn zero_gear_extern(stock: &Payload) -> Result<Vec<u8>> {
    let mut out = stock.0.clone();
    for to in [0x48, 0xE8, 0x188, 0x228, 0x2C8] {
        let code = &stock.0[stock.array_range(to + 0x20, 1, Some(0x80800009))?];
        let mut constants = vectors(stock, to + 0x30)?;
        let zero = u8::try_from(constants.len() / 16)
            .context("Borrowed stage has no free constant index")?;
        let mut rewritten = Vec::with_capacity(code.len());
        let mut at = 0;
        while at < code.len() {
            let op = code[at];
            let instruction = code
                .get(at..at + 1 + operand_bytes(op)?)
                .context("Truncated stock expression instruction")?;
            if (0x3C..=0x41).contains(&op) && instruction[1] == GEAR_EXTERN {
                ensure!(
                    op == 0x3D && instruction[2] == 0,
                    "Borrowed technique reads the gear extern with opcode {op:02X}"
                );
                rewritten.extend([0x34, zero]);
            } else {
                rewritten.extend_from_slice(instruction);
            }
            at += instruction.len();
        }
        if rewritten != code {
            constants.extend([0; 16]);
            array(&mut out, to + 0x20, 0x80800009, &rewritten, 1)?;
            array(&mut out, to + 0x30, 0x80800090, &constants, 16)?;
        }
    }
    size(&mut out)?;
    Ok(out)
}

/// The normal plate unpack of stock gear pixel programs (`81532DEA` row 2): scale, bias and
/// an occlusion bias added to the stored normal's third channel. D1 normal plates are two
/// channel BC5, whose third channel reads zero, which this bias would turn into 0.4 occlusion.
/// The stored bias is `0x3ECCCCCC`, one unit below the nearest float to 0.4.
const GEAR_NORMAL_UNPACK: [u32; 4] = [0x3FFF_0000, 0xBF80_0000, 0x3ECC_CCCC, 0];

fn put_lanes(rows: &mut [u8], row: usize, lanes: &[(usize, f32)]) -> Result<()> {
    for (lane, value) in lanes {
        put(rows, row * 16 + lane * 4, &value.to_le_bytes())?;
    }
    Ok(())
}

/// The gear dye vectors that a borrowed program reads from one native dye scope, which only
/// the gear renderer binds: the scope's defaults, with the draw's source dye. Stock program
/// `81532DEA` reads rows 0 and 1 as the detail transforms, row 2 as the detail normal unpack,
/// rows 9 and 17 as the colours its gearstack alpha selects between (13 and 21 being their
/// wear targets), and the x, y and z lanes of rows 10, 14, 20 and 24 as detail diffuse,
/// detail normal and detail smoothness weights. The scope defaults are debug colours with
/// no detail.
fn dye_vectors(
    defaults: &Payload,
    dye: Option<&Payload>,
    secondary: bool,
    detail: bool,
) -> Result<Vec<u8>> {
    let mut rows = vectors(defaults, 0x88)?;
    ensure!(rows.len() == 27 * 16, "Native armor dye vectors differ");
    // Source detail normals carry no occlusion channel either.
    put_lanes(&mut rows, 2, &[(2, 1.0)])?;
    let Some(dye) = dye else {
        // The overlay of stock and source gear leaves a colour unchanged under 0.25.
        for row in [9, 13, 17, 21] {
            put_lanes(&mut rows, row, &[(0, 0.25), (1, 0.25), (2, 0.25), (3, 1.0)])?;
        }
        return Ok(rows);
    };
    let transform = dye.bytes::<16>(0x70)?;
    let colour = dye.bytes::<16>(if secondary { 0xA0 } else { 0x90 })?;
    for (row, value) in [0, 1]
        .map(|row| (row, transform))
        .into_iter()
        .chain([9, 13, 17, 21].map(|row| (row, colour)))
    {
        put(&mut rows, row * 16, &value)?;
    }
    if detail {
        // Source programs apply the detail colour and smoothness fully and scale the
        // detail normal by its normal vector's x.
        let normal = dye.f32(0x80)?;
        for row in [10, 14, 20, 24] {
            put_lanes(&mut rows, row, &[(0, 1.0), (1, normal), (2, 1.0)])?;
        }
    }
    Ok(rows)
}

/// What a borrowed stage's program reads from the gear renderer's bindings.
struct GearReads {
    shader: u32,
    code: Vec<u8>,
    /// Plate texture slots (0 albedo, 1 normal, 2 gearstack) the stage leaves unbound.
    plates: Vec<u32>,
    /// Dye detail texture slots the stage leaves unbound.
    details: Vec<u32>,
    /// Dye scopes whose constant buffer the program declares.
    folds: Vec<expression::DyeScope>,
    /// The program declares no constant buffer zero.
    target_free: bool,
    /// The technique's own texture rows: slot and identity.
    bound: BTreeMap<u32, String>,
}

/// Rewrite a stage's texture rows with the gear textures added, and its resource set hash,
/// which covers every bound texture identity.
fn texture_rows(
    out: &mut Vec<u8>,
    patches: &mut Vec<(usize, String)>,
    to: usize,
    bound: &BTreeMap<u32, String>,
    gear: &BTreeMap<u32, String>,
) -> Result<()> {
    let mut identities = bound.clone();
    identities.extend(gear.iter().map(|(slot, symbol)| (*slot, symbol.clone())));
    let mut rows = Vec::new();
    let mut hash = 0xCBF29CE484222325u64;
    for (slot, identity) in &identities {
        let tag = if gear.contains_key(slot) {
            u32::MAX
        } else {
            u32::from_str_radix(identity, 16)?
        };
        rows.extend(slot.to_le_bytes().into_iter().chain(tag.to_le_bytes()));
        for byte in slot.to_le_bytes().into_iter().chain(identity.bytes()) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001B3);
        }
    }
    let start = array(out, to + 8, 0x80807211, &rows, 8)?;
    for (index, slot) in identities.keys().enumerate() {
        if let Some(symbol) = gear.get(slot) {
            patches.push((start + index * 8 + 4, symbol.clone()));
        }
    }
    put(out, to + 0x18, &hash.to_le_bytes())
}

struct StageInputs {
    textures: BTreeMap<u32, String>,
    samplers: Vec<String>,
    values: Vec<u8>,
    code: Vec<u8>,
    constants: Vec<u8>,
}

fn scope_masks(
    source: &Payload,
    globals: &expression::Globals,
    simulated: bool,
    pass: usize,
    out: &mut [u8],
) -> Result<[u32; 2]> {
    let mut masks = [0u32; 2];
    for (index, at) in [0x18, 0x1C].into_iter().enumerate() {
        let mut mask = globals.scope_mask(source.u32(at)?)?;
        mask &= !0x84;
        mask |= if simulated { 4 } else { 0x80 };
        // Every stock Shadowkeep sight material (18 found by `scan-materials 3f490c47`,
        // such as `80BA7DA2`) requests the gear dye scope (26) at +0x18 and provides the
        // plated-texture and dye scopes (25, 26) at +0x1C, which the gear draw callback keys
        // on. D1 has no scope of those names, so the mapped mask never set them.
        if pass == 16 {
            mask |= if index == 0 {
                1 << 26
            } else {
                (1 << 25) | (1 << 26)
            };
        }
        masks[index] = mask;
        put(
            out,
            at,
            &(mask | if index == 1 { 0x80000000 } else { 0 }).to_le_bytes(),
        )?;
    }
    Ok(masks)
}

impl Assembly<'_> {
    fn detail_textures(&mut self, dye: Option<&Payload>) -> Result<[String; 2]> {
        let mut detail = [String::new(), String::new()];
        if let Some(data) = dye {
            for (index, symbol) in detail.iter_mut().enumerate() {
                let tag = data.u32(0x60 + index * 4)?;
                if !matches!(tag, 0 | u32::MAX) {
                    *symbol = self.texture(tag)?;
                }
            }
        }
        Ok(detail)
    }

    fn material_inputs(&mut self, source: &Payload, from: usize) -> Result<StageInputs> {
        let mut textures = BTreeMap::new();
        for row in source.array(from + 0x10, 8, Some(0x80801B28))? {
            let tag = source.u32(row + 4)?;
            if !matches!(tag, 0 | u32::MAX) {
                textures.insert(source.u32(row)?, self.texture(tag)?);
            }
        }
        let mut samplers = Vec::new();
        for row in source.array(from + 0x48, 16, Some(0x80801ACC))? {
            samplers.push(self.sampler(source.u32(row)?)?);
        }
        let external = source.u32(from + 0x84)?;
        let values = if matches!(external, 0 | u32::MAX) {
            vectors(source, from + 0x58)?
        } else {
            self.source.constant_buffer(external)?.1
        };
        let code = source
            .array(from + 0x28, 1, Some(0x80800009))?
            .into_iter()
            .map(|at| source.u8(at))
            .collect::<Result<Vec<_>>>()?;
        let constants = vectors(source, from + 0x38)?;
        Ok(StageInputs {
            textures,
            samplers,
            values,
            code,
            constants,
        })
    }

    /// A single-level 4x4 texture of one colour.
    fn flat_texture(&mut self, symbol: &str, colour: [u8; 4]) -> Result<String> {
        if self.symbols.contains(symbol) {
            return Ok(symbol.to_owned());
        }
        self.texture_bytes(
            symbol,
            super::super::texture::Texture {
                format: 28,
                width: 4,
                height: 4,
                depth: 1,
                layers: 1,
                mips: 1,
                bytes: colour.repeat(16),
            },
        )
    }

    /// The gear resources a borrowed stage's program reads that the technique does not bind.
    fn gear_reads(&mut self, stock: &Payload, to: usize) -> Result<Option<GearReads>> {
        let shader = stock.u32(to)?;
        if matches!(shader, 0 | u32::MAX) {
            return Ok(None);
        }
        let used = stock.u32(0x18)?;
        let code = self
            .native
            .tag(self.native.reference(shader)?, None)?
            .0
            .clone();
        let (buffers, resources) = crate::tiger::shader::dxbc::declarations(&code)?;
        let bound = stock
            .array(to + 8, 8, Some(0x80807211))?
            .into_iter()
            .map(|row| Ok((stock.u32(row)?, format!("{:08X}", stock.u32(row + 4)?))))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let plated = self
            .globals
            .plated
            .context("Native plated texture scope is missing")?;
        let wanted = resources
            .iter()
            .copied()
            .filter(|slot| !bound.contains_key(slot))
            .collect::<BTreeSet<_>>();
        let plates = if used & (1 << plated) != 0 {
            [0, 1, 2]
                .into_iter()
                .filter(|slot| wanted.contains(slot))
                .collect()
        } else {
            Vec::new()
        };
        let mut scopes = Vec::new();
        for scope in self.globals.dyes {
            if used & (1 << scope.index) != 0 {
                let textures = scope
                    .textures
                    .context("Native armor dye texture program differs")?;
                scopes.push((scope, textures));
            }
        }
        let reads = GearReads {
            shader,
            code,
            folds: scopes
                .iter()
                .map(|(scope, _)| *scope)
                .filter(|scope| buffers.contains(&scope.slot))
                .collect(),
            details: scopes
                .iter()
                .flat_map(|(_, textures)| *textures)
                .filter(|slot| wanted.contains(slot))
                .collect(),
            plates,
            target_free: !buffers.contains(&0),
            bound,
        };
        Ok(
            (!reads.folds.is_empty() || !reads.plates.is_empty() || !reads.details.is_empty())
                .then_some(reads),
        )
    }

    /// The textures that replace the gear renderer's plate and dye detail bindings.
    fn gear_textures(
        &mut self,
        reads: &GearReads,
        equipment: &Equipment,
        dye: Option<&Payload>,
    ) -> Result<(BTreeMap<u32, String>, bool)> {
        let mut gear = BTreeMap::new();
        let plates = [
            &equipment.plates[0],
            &equipment.plates[1],
            &equipment.gear_stack,
        ];
        for &slot in &reads.plates {
            let plate = plates[slot as usize];
            ensure!(
                !plate.is_empty(),
                "Borrowed gear program needs an absent plate"
            );
            gear.insert(slot, plate.clone());
        }
        let mut detail = self.detail_textures(dye)?;
        let source_detail = detail.iter().all(|symbol| !symbol.is_empty());
        if !reads.details.is_empty() && !source_detail {
            // Neutral under both the detail overlay and the detail normal unpack.
            detail = [
                self.flat_texture("neutral-detail-diffuse", [64; 4])?,
                self.flat_texture("neutral-detail-normal", [128, 128, 255, 255])?,
            ];
        }
        for scope in self.globals.dyes {
            for (slot, texture) in scope.textures.into_iter().flatten().zip(&detail) {
                if reads.details.contains(&slot) {
                    gear.insert(slot, texture.clone());
                }
            }
        }
        Ok((gear, source_detail))
    }

    /// Fold the dye buffers a borrowed pixel program reads into its own constant buffer, as
    /// a private copy of the program. Returns the folded layout and the unpack rows changed.
    #[allow(clippy::too_many_arguments)]
    fn fold_dye_buffers(
        &mut self,
        stock: &Payload,
        out: &mut Vec<u8>,
        patches: &mut Vec<(usize, String)>,
        to: usize,
        reads: &GearReads,
        symbol: &str,
        dye: Option<&Payload>,
        (secondary, source_detail): (bool, bool),
    ) -> Result<(Vec<(u32, u32)>, usize)> {
        let mut values = vectors(stock, to + 0x50)?;
        let mut target = stock.u32(to + 0x80)?;
        if values.is_empty() {
            ensure!(
                reads.target_free,
                "Borrowed program declares a constant buffer its stage does not bind"
            );
            target = 0;
        }
        let unpack = GEAR_NORMAL_UNPACK.map(u32::to_le_bytes).concat();
        let mut unpacks = 0;
        if reads.plates.contains(&1) {
            for row in values.chunks_exact_mut(16).filter(|row| **row == *unpack) {
                row[8..12].copy_from_slice(&1f32.to_le_bytes());
                unpacks += 1;
            }
        }
        let mut layout = Vec::new();
        for scope in &reads.folds {
            layout.push((scope.slot, u32::try_from(values.len() / 16)?));
            let defaults = self.native.tag(scope.tag, Some(0x808071F3))?;
            values.extend(dye_vectors(&defaults, dye, secondary, source_detail)?);
        }
        if !layout.is_empty() {
            let size = u32::try_from(values.len() / 16)?;
            let code = crate::tiger::shader::dxbc::fold_constant_buffers(
                &reads.code,
                target,
                &layout,
                size,
            )?;
            let template = Template::read(&mut self.native, reads.shader)?;
            let name = format!("{symbol}-pixel");
            let assets = Program::new(Stage::Pixel, code)?.assets(&name, &template)?;
            let data = format!("{name}-bytecode");
            self.node(
                &data,
                assets.bytecode_template,
                &assets.bytecode,
                Some(&name),
                &[],
            )?;
            self.node(
                &name,
                assets.header_template,
                &assets.header,
                Some(&data),
                &[],
            )?;
            put(out, to, &u32::MAX.to_le_bytes())?;
            patches.push((to, name));
            put(out, to + 0x80, &target.to_le_bytes())?;
            out[to + 0x74] = 0x10;
        }
        if !layout.is_empty() || unpacks > 0 {
            array(out, to + 0x50, 0x80800090, &values, 16)?;
        }
        Ok((layout, unpacks))
    }

    /// Bind the gear resources of a borrowed stage itself. The stage's program reads the
    /// plates and dye scopes that only the gear renderer binds, and an imported item draws
    /// through the rigid object renderer, which left them unbound or holding another draw's
    /// resources: the item drew black and flickered. Returns the stage's evidence.
    #[allow(clippy::too_many_arguments)]
    fn bind_gear_stage(
        &mut self,
        stock: &Payload,
        out: &mut Vec<u8>,
        patches: &mut Vec<(usize, String)>,
        (stage, to): (Stage, usize),
        symbol: &str,
        equipment: &Equipment,
        dye: Option<&Payload>,
        secondary: bool,
    ) -> Result<Option<Value>> {
        let Some(reads) = self.gear_reads(stock, to)? else {
            return Ok(None);
        };
        ensure!(
            stage == Stage::Pixel,
            "Borrowed {stage:?} program reads gear resources"
        );
        let (gear, source_detail) = self.gear_textures(&reads, equipment, dye)?;
        let (layout, unpacks) = self.fold_dye_buffers(
            stock,
            out,
            patches,
            to,
            &reads,
            symbol,
            dye,
            (secondary, source_detail),
        )?;
        texture_rows(out, patches, to, &reads.bound, &gear)?;
        Ok(Some(
            json!({"stage":stage,"shader":reads.shader,"textures":gear,
            "folded_dye_buffers":layout,"normal_occlusion_bias":unpacks,"source_detail":source_detail}),
        ))
    }

    /// Copy the stock technique that draws this stage with the same vertex elements, and bind
    /// the gear resources its programs read to the converted plates and the draw's dye.
    fn borrowed_technique(
        &mut self,
        tag: u32,
        symbol: &str,
        draw: &Draw,
        equipment: &Equipment,
    ) -> Result<String> {
        let (pass, layout) = (draw.stage, draw.layout);
        let technique = BORROWED_TECHNIQUES
            .iter()
            .find(|(stage, stock, _)| *stage == pass && self.layouts.same_elements(layout, *stock))
            .map(|(.., technique)| *technique)
            .with_context(|| {
                format!(
                    "No stock technique draws stage {pass} with the elements of layout {layout}"
                )
            })?;
        let stock = self.native.tag(technique, Some(0x808071E8))?;
        let dye_tag = equipment.dyes.get(&(draw.dye / 2)).copied();
        let dye = dye_tag.map(|tag| self.source.read(tag)).transpose()?;
        let mut out = zero_gear_extern(&stock)?;
        let mut patches = Vec::new();
        let mut stages = Vec::new();
        for stage in [
            (Stage::Vertex, 0x48),
            (Stage::Hull, 0xE8),
            (Stage::Domain, 0x188),
            (Stage::Geometry, 0x228),
            (Stage::Pixel, 0x2C8),
        ] {
            stages.extend(self.bind_gear_stage(
                &stock,
                &mut out,
                &mut patches,
                stage,
                symbol,
                equipment,
                dye.as_ref(),
                !draw.dye.is_multiple_of(2),
            )?);
        }
        size(&mut out)?;
        self.approximations.insert(
            "PS4 source shaders are GCN and are not translated: each draw borrows a stock Shadowkeep gear technique for its stage and vertex layout, bound to the converted plates, the draw's dye and a gearstack rewritten in the stock convention without occlusion or emission".to_owned(),
        );
        if dye.is_none() && !stages.is_empty() {
            self.approximations
                .insert("PS4 draws without a source dye use a neutral dye colour".to_owned());
        }
        self.evidence.push(json!({"material":tag,"symbol":symbol,"pass":pass,"layout":layout,"borrowed_technique":technique,"gear_bindings":stages}));
        let template = self.class_template(0x808071E8)?;
        self.node(symbol, template, &out, None, &patches)
    }

    pub(super) fn material(
        &mut self,
        tag: u32,
        symbol: &str,
        draw: Draw,
        equipment: &Equipment,
    ) -> Result<String> {
        if self.symbols.contains(symbol) {
            return Ok(symbol.to_owned());
        }
        let Draw {
            stage: pass,
            dye,
            simulated,
            layout,
        } = draw;
        let ps4_source = self.source.platform() == 7;
        let expected_class = if ps4_source { 0x80801AD7 } else { 0x80801C32 };
        ensure!(
            self.source.reference(tag)? == expected_class,
            "Source material class differs"
        );
        if ps4_source {
            return self.borrowed_technique(tag, symbol, &draw, equipment);
        }
        let source = self.source.read(tag)?;
        let dye_tag = equipment.dyes.get(&(dye / 2)).copied();
        let dye_data = dye_tag.map(|tag| self.source.read(tag)).transpose()?;
        let detail = self.detail_textures(dye_data.as_ref())?;
        let mut out = vec![0; 0x408];
        put(&mut out, 8, &source.bytes::<32>(8)?)?;
        // Platform pipeline cache bits are not authored D3D state selectors.
        put(
            &mut out,
            0xC,
            &(source.u32(0xC)? & 0x00FF_FFFF).to_le_bytes(),
        )?;
        out[0x27] = 0;
        let decal = (pass == 1)
            .then(|| decal_state(source.u8(0x20)?))
            .transpose()?;
        if let Some((state, _)) = decal {
            out[0x20] = state;
        }
        let gbuffer = match pass {
            0 => Some(super::resource::Gbuffer::Surface),
            1 => decal.map(|(_, replace)| super::resource::Gbuffer::Decal { replace }),
            _ => None,
        };
        put(&mut out, 0x28, &u32::MAX.to_le_bytes())?;
        let masks = scope_masks(&source, &self.globals, simulated, pass, &mut out)?;
        let depth = (source.u32(0x1C)? & (1 << self.globals.depth.index) != 0)
            .then_some(self.globals.depth);
        let mut patches = Vec::new();
        let mut evidence = Vec::new();
        for (index, (from, to)) in [0x28, 0xC8, 0x168, 0x208, 0x2A8]
            .into_iter()
            .zip([0x48, 0xE8, 0x188, 0x228, 0x2C8])
            .enumerate()
        {
            put(&mut out, to, &u32::MAX.to_le_bytes())?;
            put(&mut out, to + 0x80, &u32::MAX.to_le_bytes())?;
            put(&mut out, to + 0x84, &u32::MAX.to_le_bytes())?;
            let shader = source.u32(from)?;
            if matches!(shader, 0 | u32::MAX) {
                continue;
            }
            let (stage, packed) = match index {
                0 => (Stage::Vertex, 2),
                1 => (Stage::Hull, 4),
                2 => (Stage::Domain, 6),
                3 => (Stage::Geometry, 3),
                4 => (Stage::Pixel, 1),
                _ => unreachable!(),
            };
            let StageInputs {
                mut textures,
                samplers,
                mut values,
                code,
                constants,
            } = self.material_inputs(&source, from)?;
            if pass == 16 && index == 0 {
                // The sight quad's z is offset by this row before reticle matrix six. Stock
                // sights carry a per-weapon constant (2.5 on the two stock sights whose quad
                // has this quad's dimensions, 1.2 to 25 across all 18), and the game builds
                // the matrix for that plane. D1's own 1.0 belongs to D1's projection and
                // drew the quad 2.5 times too large with a corner on the lens centre.
                let program = self.source.read(shader)?;
                let range = program.array_range(8, 1, Some(0x80800009))?;
                let row = usize::from(super::super::shader::sight_depth_row(&program.0[range])?);
                let at = row * 16;
                ensure!(
                    values.len() >= at + 16,
                    "Source sight depth row exceeds the initial vectors"
                );
                values[at..at + 4].copy_from_slice(&NATIVE_SIGHT_DEPTH.to_le_bytes());
                evidence.push(json!({"stage":"vertex","sight_depth_row":row,
                    "native_sight_depth":NATIVE_SIGHT_DEPTH,"approximation":"stock_sight_with_matching_quad"}));
            }
            let objects = source.u32(from + 0x78)? as usize;
            ensure!(
                objects <= 256,
                "Source object input count exceeds native indices"
            );
            let mut program = expression::translate(
                &code,
                constants,
                &expression::Inputs {
                    globals: &self.globals,
                    dye: dye_data.as_ref(),
                    empty_palette: equipment.dyes.is_empty(),
                    secondary: dye % 2 != 0,
                    plates: &equipment.plates,
                    detail: &detail,
                    objects,
                    object_channels: &self.object_channels,
                    outputs: values.len() / 16,
                    samplers: samplers.len(),
                    stage: packed,
                    reticle: pass == 16,
                    light: false,
                },
            )?;
            program.initialize(&values)?;
            let shader = self.shader(
                shader,
                super::resource::Shader {
                    stage,
                    gbuffer: gbuffer.filter(|_| index == 4),
                    layout,
                    depth: depth.filter(|_| index == 4),
                    reticle_mask: program.reticle_mask.filter(|_| index == 4),
                },
            )?;
            patches.push((to, shader));
            textures.extend(program.textures);
            let rows = textures
                .keys()
                .flat_map(|slot| slot.to_le_bytes().into_iter().chain(u32::MAX.to_le_bytes()))
                .collect::<Vec<_>>();
            let start = array(&mut out, to + 8, 0x80807211, &rows, 8)?;
            for (index, texture) in textures.values().enumerate() {
                patches.push((start + index * 8 + 4, texture.clone()));
            }
            // The resource set hash includes every actual bound texture identity.
            let mut hash = 0xCBF29CE484222325u64;
            for (slot, symbol) in &textures {
                for byte in slot.to_le_bytes().into_iter().chain(symbol.bytes()) {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001B3);
                }
            }
            put(&mut out, to + 0x18, &hash.to_le_bytes())?;
            array(&mut out, to + 0x20, 0x80800009, &program.code, 1)?;
            array(&mut out, to + 0x30, 0x80800090, &program.constants, 16)?;
            let rows = samplers
                .iter()
                .flat_map(|_| u32::MAX.to_le_bytes().into_iter().chain([0; 12]))
                .collect::<Vec<_>>();
            let start = array(&mut out, to + 0x40, 0x808073F3, &rows, 16)?;
            for (index, sampler) in samplers.into_iter().enumerate() {
                patches.push((start + index * 16, sampler));
            }
            array(&mut out, to + 0x50, 0x80800090, &values, 16)?;
            put(&mut out, to + 0x70, &(objects as u32).to_le_bytes())?;
            // The initialized expression also satisfies the native upload gate.
            out[to + 0x74] = if values.is_empty() { 0 } else { 0x10 };
            put(
                &mut out,
                to + 0x80,
                &(if values.is_empty() { u32::MAX } else { 0 }).to_le_bytes(),
            )?;
            evidence.push(json!({"stage":stage,"object_inputs":objects,"defaulted_source_globals":program.defaulted_globals}));
        }
        put(&mut out, 0x368, &u32::MAX.to_le_bytes())?;
        put(&mut out, 0x3E8, &u32::MAX.to_le_bytes())?;
        put(&mut out, 0x3EC, &u32::MAX.to_le_bytes())?;
        size(&mut out)?;
        let template = self.class_template(0x808071E8)?;
        self.evidence.push(json!({"material":tag,"symbol":symbol,"pass":pass,"dye":dye_tag,"secondary":dye%2!=0,"simulated":simulated,"scopes":masks,"stages":evidence}));
        self.node(symbol, template, &out, None, &patches)
    }
}
