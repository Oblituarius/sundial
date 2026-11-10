use super::super::expression;
use super::*;
use crate::tiger::shader::{
    identity::Stage,
    program::{Program, Template},
};

pub(super) struct Equipment {
    pub plates: [String; 3],
    /// The gearstack plate in the stock gear convention, for PS4 draws.
    pub gear_stack: String,
    pub dyes: BTreeMap<u8, u32>,
}

/// What the source gear-dye extern holds when a draw selects an unset local dye slot.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Retained {
    /// No earlier draw of the component and stage wrote a dye, so the zeroed allocation remains.
    Zeroed,
    /// Every earlier write stored this local slot's dye and color lane.
    Dye { slot: u8, secondary: bool },
    /// Earlier writes differ, or none of them passes the same draw filters as this draw.
    Unresolved,
}

impl Equipment {
    /// What the gear-dye extern holds for each unset-slot draw of a source model, by stage, dye
    /// byte and material. Source `00862CB2` allocates the extern zeroed for each art component and
    /// stage, and `007F8010` draws the component's meshes in order. `00884FE0` rewrites it when the
    /// dye byte changes: channels 0 to 2 copy their own local slot, channel 4 copies slot 0, and an
    /// unset slot writes only specular and normal W. `007F7EA0` draws a part only if its group
    /// (`+1F`) and flags (`+1C`) pass the view's filters, so an earlier write certainly precedes a
    /// draw only when it shares both. Per-part visibility bits (`+1A`) are taken as set, as in the
    /// default appearance.
    pub fn retained(
        &self,
        source: &Payload,
        meshes: &[usize],
    ) -> Result<BTreeMap<(usize, u8, u32), Retained>> {
        let mut result = BTreeMap::new();
        for stage in 0..19 {
            let mut writes = Vec::new();
            for &mesh in meshes {
                let parts = source.array(mesh + 0x48, 36, Some(0x80801AEF))?;
                let range = usize::from(source.u16(mesh + 0x58 + stage * 2)?)
                    ..usize::from(source.u16(mesh + 0x5A + stage * 2)?);
                for &part in parts
                    .get(range)
                    .context("Source stage exceeds its draw records")?
                {
                    let byte = source.u8(part + 0x1E)?;
                    let channel = byte >> 1;
                    let filter = (source.u16(part + 0x1C)?, source.u8(part + 0x1F)?);
                    let write = match channel {
                        0..=2 => Some(channel),
                        4 => Some(0),
                        _ => None,
                    }
                    .filter(|slot| self.dyes.contains_key(slot));
                    if let Some(slot) = write {
                        // Channel 4 stores the primary color at +40.
                        writes.push((filter, (slot, channel != 4 && byte & 1 != 0)));
                        continue;
                    }
                    if self.dyes.contains_key(&channel) {
                        continue;
                    }
                    let state = match writes.first() {
                        // Other channels do not share this extern's absent-slot path.
                        _ if channel > 2 && !self.dyes.is_empty() => Retained::Unresolved,
                        _ if channel > 2 => Retained::Zeroed,
                        None => Retained::Zeroed,
                        Some(&(_, (slot, secondary)))
                            if writes.iter().all(|w| w.1 == (slot, secondary))
                                && writes.iter().any(|w| w.0 == filter) =>
                        {
                            Retained::Dye { slot, secondary }
                        }
                        Some(_) => Retained::Unresolved,
                    };
                    let entry = result
                        .entry((stage, byte, source.u32(part)?))
                        .or_insert(state);
                    if *entry != state {
                        *entry = Retained::Unresolved;
                    }
                }
            }
        }
        Ok(result)
    }
}

pub(super) struct Draw {
    pub stage: usize,
    pub dye: u8,
    pub simulated: bool,
    pub layout: u16,
    /// The extern this draw sees if its local dye slot is unset.
    pub retained: Retained,
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
///
/// Every vertex program here reads a bone's three rows at `cb11[8 + 3 * bone]`, which is how
/// the imported item's renderer packs them. Some stock vertex programs instead read four rows
/// at `cb11[8 + 4 * bone]` (`80C0CEC3`, `80BFDF96`, `815B8609`): those draw correctly only
/// on bone 0, and Twilight Garrison's pistons, rigid on bone 11, never appeared in game while
/// stage 0 on layout 143 borrowed `80BDFD59` (`80C0CEC3`). The table takes the next most drawn
/// three-row technique instead (`80C1CB27`, `80C1CB26`, `80C1CB24`). Layout 143 has none at
/// stages 0 and 10, so its draws use layout 139's ([`Layouts::feeds`]).
const BORROWED_TECHNIQUES: [(usize, u16, u32); 28] = [
    (0, 30, 0x80EFAC1B),
    (0, 123, 0x80FC7DF6),
    (0, 138, 0x80C1D687),
    (0, 139, 0x80EF4DC0),
    (1, 30, 0x80C1CB27),
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
    (10, 30, 0x80C1CB26),
    (10, 138, 0x80C1CAFE),
    (10, 139, 0x80C1CB24),
    (12, 30, 0x80EFAD53),
    (12, 138, 0x80EFAD50),
    (12, 139, 0x80EFB192),
    (12, 143, 0x80EF4E84),
    (14, 139, 0x80EF5148),
];

/// PS4 draw stages whose materials lift their own GCN programs: decals additive (6),
/// transparents (7), distortion (8), light shaft occlusion (9) and lens flares (11). Their
/// effect programs (scrolling emission, arcs, glows) have no stock equivalent. Gbuffer,
/// decal, shadow and depth stages keep borrowing stock gear techniques, which bind the
/// Shadowkeep gear plates and dyes.
const LIFTED_STAGES: [usize; 5] = [6, 7, 8, 9, 11];

/// The sight reticle stage. Its programs carry the reticle projection, the sight depth row and
/// the reticle mask load the Xbox One path rewrites, and no stock gear technique draws it, so a
/// PS4 reticle draw always lifts its GCN programs and has no borrowed fallback.
pub(super) const RETICLE_STAGE: usize = 16;

/// The sight lens stage. Native reticle pixel programs discard every pixel whose reticle mask
/// lacks bit 2, which a native sight gets from a stage-14 lens drawn in front of it. A Rise of
/// Iron sight sets the bit through its own draws' render state and has no stage-14 draw, so
/// its reticle draws get a lens over their own faces (the D2 importer's `lenses` does the same
/// for source reticles without one).
pub(super) const LENS_STAGE: usize = 14;

/// The stock gear lens material borrowed for stage 14 on layout 139 (`BORROWED_TECHNIQUES`):
/// no pixel outputs beyond the stencil, no textures and no expression.
const LENS_MATERIAL: u32 = 0x80EF5148;
/// Stock sight materials `80BA7DA2` and `80EF364B`, whose reticle quads match the D1
/// quad's dimensions, subtract this from the quad's z before the reticle projection. PS4 sights
/// and ported reticles draw at it; Xbox One sights keep their source depth (`sight_depth`).
const NATIVE_SIGHT_DEPTH: f32 = 2.5;
/// A stock sight reticle material (also the source of [`NATIVE_SIGHT_DEPTH`]).
const RETICLE_MATERIAL: u32 = 0x80BA7DA2;

/// Rise of Iron sight reticles that Shadowkeep ported: the source reticle's pixel program and
/// slot-0 dot texture, the stock technique drawing the port, and the input channel names of the
/// stock model component that technique was authored for (object input `n` reads name `n`).
///
/// The First Curse's reticle (`80A734AE`: pixel program `80A3D246`, dot `80A3D241`, flare
/// `80A743B4`) is The Last Word's (`815287EC`, component `815287EF`, models `815287EE` and
/// `81A6AAD1`). The stock pixel expression is the source one with the same constants, the same
/// two object inputs (`0E36F79F` power glow, `79FD498A` visibility) and Shadowkeep's HDR output
/// reads (extern `5C`, which only reticle techniques read, always with extern `49`), and it binds
/// the same dot and flare pair redrawn (`80BC5C5C`, `815287EB`, plain 2D textures).
/// The lifted D1 programs never drew the dot in game; the stock port draws it as Shadowkeep does.
const PORTED_RETICLES: [(u32, u32, u32, &[u32]); 1] = [(
    0x80A3D246,
    0x80A3D241,
    0x815287EC,
    &[0x0E36F79F, 0x32CC5402, 0x79FD498A, 0x9F7F13A9, 0xF061CA4E],
)];

/// A reticle vertex expression's tail: extern `0x49` field 4 (the reticle viewport) into one
/// constant row. The client sets extern `0x49` only for the reticle stage, so the lens drops
/// it and that row keeps the material's own value.
const RETICLE_VIEWPORT_READ: [u8; 3] = [0x3D, 0x49, 4];
/// A native material's vertex and pixel stage blocks.
const VERTEX: usize = 0x48;
const PIXEL: usize = 0x2C8;

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
        if pass == RETICLE_STAGE {
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

/// The native stage and packed stage number of a material's shader slot.
fn shader_slot(index: usize) -> (Stage, u8) {
    match index {
        0 => (Stage::Vertex, 2),
        1 => (Stage::Hull, 4),
        2 => (Stage::Domain, 6),
        3 => (Stage::Geometry, 3),
        4 => (Stage::Pixel, 1),
        _ => unreachable!(),
    }
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

    /// Carry a source vertex offset. Twilight Garrison's D1 vertex program `80A0770A` adds
    /// `(COLOR + cb[0]) * cb[1]` to the object-space position, with cb[0] a fixed bias and
    /// cb[1] written by its expression from the model's object inputs: piston material
    /// `80A7CD31` writes `saturate(CD6FA091) * (0.11 * gender + 0.12 * (1 - gender))`, where
    /// CD6FA091 is a curve over the player's 5C6FEDF6 (FNV-1 "linear_speed") that both
    /// versions' player banks compute (D1 `80A74674`, Shadowkeep `80C45411`). So the pistons
    /// extend as the player moves, and the ring glows light past 2. A constant scale is
    /// recorded in `vertex_bakes` for the geometry to apply, and the borrowed program draws the
    /// stored positions. Otherwise the borrowed vertex program gets the offset added
    /// (`dxbc::add_vertex_offset`), its buffer written by the translated expression. A program
    /// without a colour input, such as gear program `81532FC5`, first gets one
    /// (`dxbc::add_input`): the draw's layout feeds it. Stock `80C0CEC3` has the same offset
    /// built in but reads four bone rows (see `BORROWED_TECHNIQUES`), so it is not borrowed.
    /// Returns the stage's evidence.
    fn vertex_offset(
        &mut self,
        tag: u32,
        stock: &Payload,
        out: &mut Vec<u8>,
        patches: &mut Vec<(usize, String)>,
        symbol: &str,
    ) -> Result<Option<Value>> {
        const BIAS: [f32; 4] = [-0.5, -0.5, -0.5, 1.0];
        let (from, to) = (0x28, 0x48);
        let source = self.source.read(tag)?;
        let code = source
            .array(from + 0x28, 1, Some(0x80800009))?
            .into_iter()
            .map(|at| source.u8(at))
            .collect::<Result<Vec<_>>>()?;
        if code.is_empty() {
            return Ok(None);
        }
        let values = vectors(&source, from + 0x58)?;
        let bias = BIAS.map(f32::to_le_bytes).concat();
        if !(matches!(source.u32(from + 0x84)?, 0 | u32::MAX)
            && source.array(from + 0x10, 8, Some(0x80801B28))?.is_empty()
            && source.array(from + 0x48, 16, Some(0x80801ACC))?.is_empty()
            && values.len() == 32
            && values[..16] == *bias)
        {
            self.approximations.insert(format!(
                "PS4 source vertex expressions other than the stock vertex offset are not translated (material {tag:08X})"
            ));
            return Ok(None);
        }
        let shader = stock.u32(to)?;
        let objects = source.u32(from + 0x78)? as usize;
        ensure!(
            objects <= 256,
            "Source object input count exceeds native indices"
        );
        let mut program = expression::translate(
            &code,
            vectors(&source, from + 0x38)?,
            &expression::Inputs {
                globals: &self.globals,
                dye: expression::Dye::Unset(None),
                plates: &[String::new(), String::new(), String::new()],
                detail: &[String::new(), String::new()],
                objects,
                object_channels: &self.object_channels,
                outputs: 2,
                samplers: 0,
                stage: 2,
                reticle: false,
                light: false,
            },
        )?;
        program.initialize(&values)?;
        ensure!(
            program.textures.is_empty(),
            "Source vertex expression binds textures"
        );
        // An expression without object or extern inputs has a constant scale. The geometry then
        // stores the offset vertices (`geometry::bake_vertex_offsets`) and every pass draws them
        // with a zero runtime scale, so the result needs no expression to run in game.
        if let Some(scale) = program
            .constant_outputs(&values)
            .and_then(|outputs| outputs.get(1).copied())
        {
            self.vertex_bakes
                .insert(symbol.to_owned(), [scale[0], scale[1], scale[2]]);
            self.approximations.insert(
                "Constant source vertex offsets are stored in the converted vertex positions"
                    .to_owned(),
            );
            // The borrowed program, which has no offset, draws the stored positions.
            return Ok(Some(
                json!({"stage":"vertex","shader":shader,"baked_scale":scale,"offset_added":false}),
            ));
        }
        let bytecode = self
            .native
            .tag(self.native.reference(shader)?, None)?
            .0
            .clone();
        let (buffers, _) = crate::tiger::shader::dxbc::declarations(&bytecode)?;
        ensure!(
            !buffers.contains(&0),
            "Borrowed vertex program {shader:08X} has its own constant buffer"
        );
        let (bytecode, colour) = match crate::tiger::shader::dxbc::signature(&bytecode, b"ISGN")?
            .into_iter()
            .find(|(semantic, index, ..)| semantic == "COLOR" && *index == 0)
        {
            Some(input) => (bytecode, input.2),
            None => crate::tiger::shader::dxbc::add_input(&bytecode, "COLOR", 0)?,
        };
        let code = crate::tiger::shader::dxbc::add_vertex_offset(&bytecode, colour, 0)?;
        let template = Template::read(&mut self.native, shader)?;
        let name = format!("{symbol}-vertex");
        let assets = Program::new(Stage::Vertex, code)?.assets(&name, &template)?;
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
        array(out, to + 0x20, 0x80800009, &program.code, 1)?;
        array(out, to + 0x30, 0x80800090, &program.constants, 16)?;
        array(out, to + 0x50, 0x80800090, &values, 16)?;
        put(out, to + 0x70, &(objects as u32).to_le_bytes())?;
        out[to + 0x74] = 0x10;
        put(out, to + 0x80, &0u32.to_le_bytes())?;
        put(out, to + 0x84, &u32::MAX.to_le_bytes())?;
        Ok(Some(
            json!({"stage":"vertex","shader":shader,"offset_added":true,"object_inputs":objects,
            "defaulted_source_globals":program.defaulted_globals}),
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
        let borrowed = |fits: &dyn Fn(u16) -> bool| {
            BORROWED_TECHNIQUES
                .iter()
                .find(|(stage, stock, _)| *stage == pass && fits(*stock))
                .map(|(.., technique)| *technique)
        };
        let technique = borrowed(&|stock| self.layouts.same_elements(layout, stock))
            .or_else(|| borrowed(&|stock| self.layouts.feeds(layout, stock)))
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
        stages.extend(self.vertex_offset(tag, &stock, &mut out, &mut patches, symbol)?);
        size(&mut out)?;
        self.approximations.insert(
            "PS4 surface, shadow and depth draws, and effect draws whose GCN program does not lift, borrow a stock Shadowkeep gear technique for their stage and vertex layout, bound to the converted plates, the draw's dye and a gearstack rewritten in the stock convention without occlusion or emission".to_owned(),
        );
        if dye.is_none() && !stages.is_empty() {
            self.approximations
                .insert("PS4 draws without a source dye use a neutral dye colour".to_owned());
        }
        self.evidence.push(json!({"material":tag,"symbol":symbol,"pass":pass,"layout":layout,"borrowed_technique":technique,"gear_bindings":stages}));
        let template = self.class_template(0x808071E8)?;
        self.node(symbol, template, &out, None, &patches)
    }

    /// The stage-14 lens over the reticle material `reticle`: the stock lens's render state and
    /// pixel side with the reticle's own vertex side, so the lens covers exactly the reticle's
    /// pixels and sets their mask bit.
    /// Every stock reticle draw (1,410 stage-16 parts, `stagehdr`, 2026-11-06) has header word
    /// `+0x14` = `0x10000200`, a value no other stage uses, and no opaque bit (31) in its `+0x1C`
    /// scopes, which only opaque stages set. The word requests per-draw resources: stock reticle
    /// pixel programs read the sight's lens mask from the slot after their own textures, and
    /// D1's `0x800` is no Shadowkeep request. A reticle keeps its source state selector
    /// (`+0x0C`: D1 sights use 6, as stock `815287EC` does) and its mapped scopes.
    /// The Xbox One path keeps its source state.
    fn native_reticle_state(&mut self, pass: usize, out: &mut [u8]) -> Result<()> {
        if pass != RETICLE_STAGE || self.source.platform() != 7 {
            return Ok(());
        }
        let shell = self.native.tag(RETICLE_MATERIAL, Some(0x808071E8))?;
        ensure!(
            shell.u32(8)? == 1
                && shell.u32(24)? == 0x0400_2083
                && shell.u32(28)? == 0x0600_2083
                && shell.u32(32)? == 0x88,
            "Native reticle material contract differs"
        );
        if super::super::shader::debug_reticle_unmasked() {
            self.approximations.insert(
                "DIAGNOSTIC BUILD: reticle pixels drawn without the lens mask. Not for normal use."
                    .to_owned(),
            );
        }
        out[0x10..0x18].copy_from_slice(&shell.0[0x10..0x18]);
        let scopes = u32::from_le_bytes(out[0x1C..0x20].try_into()?) & 0x7FFF_FFFF;
        put(out, 0x1C, &scopes.to_le_bytes())
    }

    /// The stock technique of a PS4 reticle that Shadowkeep ported ([`PORTED_RETICLES`]), copied
    /// whole: its programs, render state, scopes, textures and expressions. Its object inputs
    /// are re-indexed by channel name into this component's inputs, and its sight depth row
    /// carries the plane of the source quad. `None` when the reticle has no port.
    fn ported_reticle(&mut self, tag: u32, symbol: &str) -> Result<Option<String>> {
        if self.source.platform() != 7 {
            return Ok(None);
        }
        let source = self.source.read(tag)?;
        let pixel = source.u32(0x2A8)?;
        let mut dot = None;
        for row in source.array(0x2A8 + 0x10, 8, Some(0x80801B28))? {
            if source.u32(row)? == 0 {
                dot = Some(source.u32(row + 4)?);
            }
        }
        let Some((technique, inputs)) = PORTED_RETICLES
            .iter()
            .find(|(program, texture, ..)| *program == pixel && Some(*texture) == dot)
            .map(|(.., technique, inputs)| (*technique, *inputs))
        else {
            return Ok(None);
        };
        let stock = self.native.tag(technique, Some(0x808071E8))?;
        ensure!(
            stock.u32(0x14)? == 0x1000_0200
                && stock.u32(0x1C)? >> 31 == 0
                && [0xE8, 0x188, 0x228].into_iter().all(|to| stock
                    .u32(to)
                    .is_ok_and(|shader| matches!(shader, 0 | u32::MAX)))
                && stock.u32(VERTEX + 0x70)? == 0
                && stock.u32(PIXEL + 0x70)? as usize == inputs.len(),
            "Ported reticle technique {technique:08X} differs from its contract"
        );
        let mut out = stock.0.clone();
        // The stock quad's plane (2.25 for The Last Word) belongs to the stock quad; the source
        // quad's dimensions are those of the stock sights drawn at `NATIVE_SIGHT_DEPTH`.
        let vertex = stock.u32(VERTEX)?;
        let bytecode = self.native.tag(self.native.reference(vertex)?, None)?;
        let row = usize::from(super::super::shader::sight_depth_row(&bytecode.0)?);
        let values = stock.array_range(VERTEX + 0x50, 16, Some(0x80800090))?;
        let at = values.start + row * 16;
        ensure!(
            at + 16 <= values.end,
            "Ported reticle sight depth row exceeds its initial vectors"
        );
        put(&mut out, at, &NATIVE_SIGHT_DEPTH.to_le_bytes())?;
        // Object input `n` reads this component's input `n`: re-index each read by name.
        let code = stock.array_range(PIXEL + 0x20, 1, Some(0x80800009))?;
        let mut remapped = Vec::new();
        let mut at = code.start;
        while at < code.end {
            let op = stock.0[at];
            let length = 1 + operand_bytes(op)?;
            ensure!(
                at + length <= code.end,
                "Truncated stock expression instruction"
            );
            ensure!(
                !(0x3C..=0x41).contains(&op) || stock.0[at + 1] != GEAR_EXTERN,
                "Ported reticle technique reads the gear extern"
            );
            if op == 0x4D {
                let name = *inputs
                    .get(usize::from(stock.0[at + 1]))
                    .context("Ported reticle object input exceeds its stock inputs")?;
                let index = self
                    .object_channels
                    .iter()
                    .position(|channel| *channel == name)
                    .with_context(|| {
                        format!("Sight component has no input {name:08X} for its ported reticle")
                    })?;
                out[at + 1] = u8::try_from(index)?;
                remapped.push(
                    json!({"channel":format!("{name:08X}"),"stock":stock.0[at + 1],"input":index}),
                );
            }
            at += length;
        }
        put(
            &mut out,
            PIXEL + 0x70,
            &u32::try_from(self.object_channels.len())?.to_le_bytes(),
        )?;
        self.approximations.insert(
            "PS4 sight reticles that Shadowkeep ported draw with the stock port's technique and textures".to_owned(),
        );
        self.evidence
            .push(json!({"material":tag,"symbol":symbol,"pass":RETICLE_STAGE,
            "ported_reticle":format!("{technique:08X}"),"sight_depth_row":row,
            "native_sight_depth":NATIVE_SIGHT_DEPTH,"object_inputs":remapped}));
        let template = self.class_template(0x808071E8)?;
        self.node(symbol, template, &out, None, &[]).map(Some)
    }

    pub(super) fn reticle_lens(&mut self, reticle: &str) -> Result<String> {
        let symbol = format!("{reticle}-lens");
        if self.symbols.contains(&symbol) {
            return Ok(symbol);
        }
        let node = self
            .nodes
            .iter()
            .find(|node| node["symbol"] == reticle)
            .context("Reticle material for its lens")?
            .clone();
        let mut out = fs::read(
            self.folder
                .join(node["file"].as_str().context("Reticle material file")?),
        )?;
        let reticle_material = Payload(out.clone());
        let shell = self.native.tag(LENS_MATERIAL, Some(0x808071E8))?;
        ensure!(
            shell.u32(8)? == 1
                && shell.u32(24)? == 0x83
                && shell.u32(28)? == 0x0200_0083
                && shell.u32(32)? == 0
                && !matches!(shell.u32(PIXEL)?, 0 | u32::MAX)
                && [8, 0x20, 0x30, 0x40, 0x50]
                    .into_iter()
                    .all(|field| shell.u64(PIXEL + field).ok() == Some(0)),
            "Native lens material contract differs"
        );
        // The stock lens's state and scopes, and its whole pixel side, whose arrays are empty.
        out[8..0x48].copy_from_slice(&shell.0[8..0x48]);
        out[PIXEL..PIXEL + 0xA0].copy_from_slice(&shell.0[PIXEL..PIXEL + 0xA0]);
        // Without the reticle stage's viewport read the vertex constants keep their values.
        let code = reticle_material.array(VERTEX + 0x20, 1, Some(0x80800009))?;
        let bytes = code
            .iter()
            .map(|&at| reticle_material.0[at])
            .collect::<Vec<_>>();
        let tail = bytes.len().checked_sub(5);
        ensure!(
            tail.is_some_and(|tail| bytes[tail..tail + 3] == RETICLE_VIEWPORT_READ
                && bytes[tail + 3] == 0x43
                && !bytes[..tail].windows(2).any(|pair| pair == [0x3D, 0x49])),
            "Reticle vertex expression differs from the lens contract"
        );
        put(
            &mut out,
            VERTEX + 0x20,
            &(tail.context("Reticle vertex expression")? as u64).to_le_bytes(),
        )?;
        // Pixel texture and sampler rows of the reticle stay in the payload unreferenced, so
        // their relocations go with them.
        let dropped = [8, 0x40]
            .into_iter()
            .map(|field| {
                let stride = if field == 8 { 8 } else { 16 };
                Ok(reticle_material
                    .array(PIXEL + field, stride, None)?
                    .into_iter()
                    .map(move |at| at..at + stride))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let patches = node["patches"]
            .as_array()
            .context("Reticle material relocations")?
            .iter()
            .map(|patch| {
                Ok((
                    usize::try_from(patch["offset"].as_u64().context("Relocation offset")?)?,
                    patch["symbol"]
                        .as_str()
                        .context("Relocation symbol")?
                        .to_owned(),
                ))
            })
            .collect::<Result<Vec<(usize, String)>>>()?
            .into_iter()
            .filter(|(offset, _)| {
                *offset != PIXEL && !dropped.iter().any(|range| range.contains(offset))
            })
            .collect::<Vec<_>>();
        for (offset, _) in &patches {
            ensure!(
                out.get(*offset..offset + 4) == Some(&u32::MAX.to_le_bytes()[..]),
                "Reticle lens relocation lost its placeholder"
            );
        }
        if let Some(bake) = self.vertex_bakes.get(reticle).copied() {
            self.vertex_bakes.insert(symbol.clone(), bake);
        }
        let template = u32::try_from(node["template"].as_u64().context("Reticle template")?)?;
        self.evidence
            .push(json!({"material":reticle,"symbol":symbol,"pass":LENS_STAGE,
            "lens_material":format!("{LENS_MATERIAL:08X}"),
            "approximation":"reticle_lens_over_reticle_faces"}));
        self.node(&symbol, template, &out, None, &patches)
    }

    /// The sight quad's z is offset by this row before view matrix six, which both builds fill
    /// from the corresponding camera block. The depth is each weapon's reticle size (0.15 on a
    /// Rise of Iron sidearm to 26 on its snipers, 1.2 to 25 across stock Shadowkeep sights), so
    /// the source value is kept.
    fn sight_depth(&mut self, shader: u32, values: &[u8]) -> Result<Value> {
        let program = self.source.read(shader)?;
        let range = program.array_range(8, 1, Some(0x80800009))?;
        let row = usize::from(super::super::shader::sight_depth_row(&program.0[range])?);
        let at = row * 16;
        ensure!(
            values.len() >= at + 16,
            "Source sight depth row exceeds the initial vectors"
        );
        let depth = f32::from_le_bytes(values[at..at + 4].try_into()?);
        ensure!(
            depth.is_finite() && depth > 0.0,
            "Source sight depth is not positive"
        );
        Ok(json!({"stage":"vertex","sight_depth_row":row,"sight_depth":depth}))
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
        let ps4_source = self.source.platform() == 7;
        let expected_class = if ps4_source { 0x80801AD7 } else { 0x80801C32 };
        ensure!(
            self.source.reference(tag)? == expected_class,
            "Source material class differs"
        );
        if ps4_source {
            if draw.stage == RETICLE_STAGE
                && let Some(symbol) = self.ported_reticle(tag, symbol)?
            {
                return Ok(symbol);
            }
            if (LIFTED_STAGES.contains(&draw.stage) || draw.stage == RETICLE_STAGE)
                && let Some(symbol) = self.lifted_material(tag, symbol, &draw, equipment)?
            {
                return Ok(symbol);
            }
            if draw.stage == RETICLE_STAGE {
                let reason = self
                    .evidence
                    .last()
                    .and_then(|row| row["gcn_lift_error"].as_str())
                    .unwrap_or("unknown")
                    .to_owned();
                anyhow::bail!("PS4 sight reticle program does not lift: {reason}");
            }
            return self.borrowed_technique(tag, symbol, &draw, equipment);
        }
        self.source_material(tag, symbol, draw, equipment, None)
    }

    /// Convert a PS4 material whose vertex and pixel programs lift from GCN through the
    /// path Xbox One materials take. Any failure leaves no trace in the graph, records why,
    /// and returns `None` so the draw borrows a stock technique instead.
    fn lifted_material(
        &mut self,
        tag: u32,
        symbol: &str,
        draw: &Draw,
        equipment: &Equipment,
    ) -> Result<Option<String>> {
        let source = self.source.read(tag)?;
        let (vertex, pixel) = (source.u32(0x28)?, source.u32(0x2A8)?);
        let nodes = self.nodes.len();
        let symbols = self.symbols.clone();
        let evidence = self.evidence.len();
        let approximations = self.approximations.clone();
        let bakes = self.vertex_bakes.clone();
        let outcome = (|| -> Result<String> {
            ensure!(
                [0xC8, 0x168, 0x208]
                    .into_iter()
                    .map(|at| source.u32(at))
                    .collect::<Result<Vec<_>>>()?
                    .iter()
                    .all(|shader| matches!(*shader, 0 | u32::MAX))
                    && !matches!(vertex, 0 | u32::MAX)
                    && !matches!(pixel, 0 | u32::MAX),
                "Only vertex and pixel programs lift"
            );
            let elements = self.layouts.ordered(draw.layout)?;
            // Pixel texture slots bound to cube maps, which the lift samples as cubes.
            let mut cubes = std::collections::BTreeSet::new();
            for row in source.array(0x2A8 + 0x10, 8, Some(0x80801B28))? {
                let texture = source.u32(row + 4)?;
                if !matches!(texture, 0 | u32::MAX) && self.source.is_cube_texture(texture)? {
                    cubes.insert(u8::try_from(source.u32(row)?)?);
                }
            }
            let lifted = super::super::gcn::lift(self.source, vertex, pixel, &elements, &cubes)?;
            for (name, text) in [
                format!("gcn-{vertex:08X}-layout-{}.hlsl", draw.layout),
                format!("gcn-{pixel:08X}-vs-{vertex:08X}.hlsl"),
            ]
            .into_iter()
            .zip(&lifted.hlsl)
            {
                fs::write(self.folder.join(name), text)?;
            }
            self.source_material(
                tag,
                symbol,
                Draw { ..*draw },
                equipment,
                Some((&lifted, vertex)),
            )
        })();
        match outcome {
            Ok(symbol) => {
                self.evidence
                    .push(json!({"material":tag,"symbol":symbol,"pass":draw.stage,
                    "layout":draw.layout,"gcn_lifted":{"vertex":vertex,"pixel":pixel}}));
                Ok(Some(symbol))
            }
            Err(error) => {
                crate::cancellation::check()?;
                self.nodes.truncate(nodes);
                self.symbols = symbols;
                self.evidence.truncate(evidence);
                self.approximations = approximations;
                self.vertex_bakes = bakes;
                self.evidence.push(
                    json!({"material":tag,"pass":draw.stage,"layout":draw.layout,
                    "gcn_lift_error":format!("{error:#}")}),
                );
                self.approximations.insert(
                    "PS4 effect materials whose GCN programs do not lift (cube maps, loops, cross-lane reads) borrow a stock technique".to_owned(),
                );
                Ok(None)
            }
        }
    }

    /// Convert a source material with its own programs: Xbox One SM5 bytecode, or the SM5
    /// a PS4 material's GCN programs lifted to (`lifted`, with the vertex program's tag).
    fn source_material(
        &mut self,
        tag: u32,
        symbol: &str,
        draw: Draw,
        equipment: &Equipment,
        lifted: Option<(&super::super::gcn::Lifted, u32)>,
    ) -> Result<String> {
        let Draw {
            stage: pass,
            dye,
            simulated,
            layout,
            retained,
        } = draw;
        let source = self.source.read(tag)?;
        // The palette holds one dye per local slot. A draw through an unset slot reads what an
        // earlier draw of its component and stage left in the gear-dye extern.
        let selected = equipment.dyes.get(&(dye / 2)).copied();
        let (dye_tag, secondary) = match (selected, retained) {
            (Some(tag), _) => (Some(tag), dye % 2 != 0),
            (None, Retained::Dye { slot, secondary }) => {
                (equipment.dyes.get(&slot).copied(), secondary)
            }
            (None, _) => (None, false),
        };
        let dye_data = dye_tag.map(|tag| self.source.read(tag)).transpose()?;
        let detail = self.detail_textures(dye_data.as_ref())?;
        let dye_input = match (selected, retained, dye_data.as_ref()) {
            (Some(_), _, Some(dye)) => expression::Dye::Selected { dye, secondary },
            (None, Retained::Zeroed | Retained::Dye { .. }, data) => {
                expression::Dye::Unset(data.map(|dye| (dye, secondary)))
            }
            _ => expression::Dye::Unresolved,
        };
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
        self.native_reticle_state(pass, &mut out)?;
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
            let (stage, packed) = shader_slot(index);
            let StageInputs {
                mut textures,
                samplers,
                mut values,
                code,
                constants,
            } = self.material_inputs(&source, from)?;
            if pass == RETICLE_STAGE && index == 0 && self.source.platform() == 7 {
                // The sight quad's z is offset by this row before reticle matrix six. Stock
                // sights carry a per-weapon constant (2.5 on the two stock sights whose quad
                // has this quad's dimensions, 1.2 to 25 across all 18), and the game builds
                // the matrix for that plane. The PS4 sights were verified in game at the stock
                // depth, so they keep it; the Xbox One path keeps the source depth below.
                // A lifted PS4 sight reads the row from its lifted vertex program.
                let row = usize::from(match lifted {
                    Some((programs, _)) => super::super::shader::sight_depth_row(&programs.vertex)?,
                    None => {
                        let program = self.source.read(shader)?;
                        let range = program.array_range(8, 1, Some(0x80800009))?;
                        super::super::shader::sight_depth_row(&program.0[range])?
                    }
                });
                let at = row * 16;
                ensure!(
                    values.len() >= at + 16,
                    "Source sight depth row exceeds the initial vectors"
                );
                values[at..at + 4].copy_from_slice(&NATIVE_SIGHT_DEPTH.to_le_bytes());
                evidence.push(json!({"stage":"vertex","sight_depth_row":row,
                    "native_sight_depth":NATIVE_SIGHT_DEPTH,"approximation":"stock_sight_with_matching_quad"}));
            } else if pass == RETICLE_STAGE && index == 0 {
                evidence.push(self.sight_depth(shader, &values)?);
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
                    dye: dye_input,
                    plates: &equipment.plates,
                    detail: &detail,
                    objects,
                    object_channels: &self.object_channels,
                    outputs: values.len() / 16,
                    samplers: samplers.len(),
                    stage: packed,
                    reticle: pass == RETICLE_STAGE,
                    light: false,
                },
            )?;
            program.initialize(&values)?;
            let lifted = match (lifted, index) {
                (None, _) => None,
                (Some((programs, _)), 0) => {
                    Some((programs.vertex.clone(), format!("-layout-{layout}")))
                }
                (Some((programs, vertex)), 4) => {
                    Some((programs.pixel.clone(), format!("-vs-{vertex:08X}")))
                }
                (Some(_), _) => anyhow::bail!("Lifted material has a {stage:?} program"),
            };
            let shader = self.shader(
                shader,
                super::resource::Shader {
                    stage,
                    gbuffer: gbuffer.filter(|_| index == 4),
                    layout,
                    depth: depth.filter(|_| index == 4),
                    reticle_mask: program.reticle_mask.filter(|_| index == 4),
                    lifted,
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
