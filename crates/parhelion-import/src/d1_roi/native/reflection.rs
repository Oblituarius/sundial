//! The environment reflection Rise of Iron gear surfaces bake into their albedo.
//!
//! D1 gear surface programs, such as `80A38A95` (Gjallarhorn and Iron Gjallarhorn), sample a
//! cube map and add the reflection to the albedo they write to the G-buffer, before lighting:
//! for `80A38A95`, `refl = cube.rgb * cube.a * (row6.x + row5.x * B) * lerp(row7, row8, B) *
//! (1 - R)` with R and B the gearstack's red and blue, and `albedo += refl * (albedo * row9.x +
//! row9.y)`. A PS4 surface draw borrows a stock Shadowkeep gear program, which has no such term,
//! so chrome items (Iron Gjallarhorn) drew dark. 3,556 D1 programs sample cubes, the rows differ
//! between them, and many add more terms, so the reflection is measured rather than recognized:
//! the material's own GCN program runs on the CPU ([`crate::d1_roi::gcn::eval`]) over a lattice
//! of gearstack and albedo values, once with the cube at its mean colour and once black, and
//! the difference of the albedo target is added to a private copy of the albedo plate. The
//! reflection then follows the gearstack masks it reads, but not the view.
use super::material::{Draw, Equipment};
use super::*;
use crate::d1_roi::gcn::eval;

/// What a source pixel program's texture slot holds.
#[derive(Clone, Copy, Debug)]
pub(super) enum Slot {
    /// A material texture.
    Texture(u32),
    /// A material cube map.
    Cube(u32),
    /// Equipment plate 0 (albedo), 1 (normal) or 2 (gearstack).
    Plate(usize),
}

/// Lattice steps of gearstack red (the dye mask), blue, green, and the albedo level.
const STEPS: [usize; 4] = [17, 17, 5, 9];
/// A reflection below this (linear, any channel, anywhere on the lattice) leaves the plate.
const VISIBLE: f32 = 1.0 / 255.0;

/// The albedo change at each lattice point: index `((r * B + b) * G + g) * T + t`.
struct Lattice(Vec<[f32; 3]>);

impl Lattice {
    fn index(r: usize, b: usize, g: usize, t: usize) -> usize {
        ((r * STEPS[1] + b) * STEPS[2] + g) * STEPS[3] + t
    }

    /// The change for gearstack `stack` (red, green, blue) and linear albedo `albedo`,
    /// bilinear in red and blue, nearest in green and linear in the albedo level per channel.
    fn change(&self, stack: [f32; 3], albedo: [f32; 3]) -> [f32; 3] {
        let axis = |value: f32, steps: usize| {
            let x = value.clamp(0.0, 1.0) * (steps - 1) as f32;
            let low = (x.floor() as usize).min(steps - 2);
            (low, x - low as f32)
        };
        let (r, fr) = axis(stack[0], STEPS[0]);
        let (b, fb) = axis(stack[2], STEPS[1]);
        let g = (stack[1].clamp(0.0, 1.0) * (STEPS[2] - 1) as f32).round() as usize;
        std::array::from_fn(|c| {
            let (t, ft) = axis(albedo[c], STEPS[3]);
            let mut sum = 0.0;
            for (dr, wr) in [(0, 1.0 - fr), (1, fr)] {
                for (db, wb) in [(0, 1.0 - fb), (1, fb)] {
                    for (dt, wt) in [(0, 1.0 - ft), (1, ft)] {
                        sum += wr * wb * wt * self.0[Self::index(r + dr, b + db, g, t + dt)][c];
                    }
                }
            }
            sum
        })
    }
}

/// The inputs of one lattice point.
struct Probe<'a> {
    rows: &'a [[f32; 4]],
    slots: &'a BTreeMap<u32, Slot>,
    means: &'a BTreeMap<u32, [f32; 4]>,
    stack: [f32; 3],
    albedo: f32,
    cube: bool,
}

impl eval::Inputs for Probe<'_> {
    fn interpolant(&self, attribute: u8, lane: u8) -> f32 {
        // Any unit frame works: the reflection takes the cube's mean, which no direction changes.
        let k = f32::from(attribute);
        [0.3 + 0.05 * k, 0.5, 0.7 - 0.05 * k, 1.0][usize::from(lane & 3)]
    }

    fn constant(&self, slot: u8, dword: u32) -> u32 {
        let (row, lane) = ((dword / 4) as usize, (dword % 4) as usize);
        match slot {
            0 => self.rows.get(row).map_or(0, |row| row[lane].to_bits()),
            // Other buffers (view, bones, model rows) take zero.
            _ => 0,
        }
    }

    fn sample(&self, slot: u8) -> [f32; 4] {
        match self.slots.get(&u32::from(slot)) {
            Some(Slot::Plate(0)) => [self.albedo, self.albedo, self.albedo, 1.0],
            // A flat normal in the BC5 plate's unsigned encoding.
            Some(Slot::Plate(1)) => [0.5, 0.5, 0.0, 1.0],
            Some(Slot::Plate(_)) => [self.stack[0], self.stack[1], self.stack[2], 1.0],
            Some(Slot::Cube(tag)) if self.cube => self.means[tag],
            Some(Slot::Cube(_)) => [0.0; 4],
            Some(Slot::Texture(tag)) => self.means[tag],
            // An unbound slot reads zero.
            None => [0.0; 4],
        }
    }
}

fn linear(value: u8) -> f32 {
    let c = f32::from(value) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn encoded(value: f32) -> u8 {
    let c = value.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

fn decode_bc1(blocks: &[u8], edge: usize) -> Vec<[u8; 4]> {
    let columns = (edge / 4).max(1);
    let mut texels = vec![[0u8; 4]; edge * edge];
    for (index, block) in blocks.chunks_exact(8).enumerate() {
        let mut pixels = [0u8; 64];
        bcdec_rs::bc1(block, &mut pixels, 16);
        let (bx, by) = (index % columns * 4, index / columns * 4);
        for y in 0..4.min(edge) {
            for x in 0..4.min(edge) {
                let at = (y * 4 + x) * 4;
                texels[(by + y) * edge + bx + x] = pixels[at..at + 4].try_into().unwrap();
            }
        }
    }
    texels
}

/// A four-colour BC1 block for 16 RGB texels: endpoints at the extremes of the texels along
/// their principal axis, each texel the nearest of the four palette colours.
fn encode_bc1(texels: &[[f32; 3]; 16]) -> [u8; 8] {
    let mean: [f32; 3] = std::array::from_fn(|c| texels.iter().map(|t| t[c]).sum::<f32>() / 16.0);
    let mut covariance = [[0f32; 3]; 3];
    for t in texels {
        for i in 0..3 {
            for j in 0..3 {
                covariance[i][j] += (t[i] - mean[i]) * (t[j] - mean[j]);
            }
        }
    }
    // Start from the covariance column of the widest channel, which the principal axis never
    // leaves orthogonal (a start of (1, 1, 1) is when channels move against each other).
    let widest = (0..3)
        .max_by(|&a, &b| covariance[a][a].total_cmp(&covariance[b][b]))
        .unwrap_or(0);
    let mut axis = if covariance[widest][widest] > 1e-6 {
        covariance[widest]
    } else {
        [1.0, 1.0, 1.0]
    };
    let length = axis.iter().map(|v| v * v).sum::<f32>().sqrt();
    axis = axis.map(|v| v / length);
    for _ in 0..8 {
        let next: [f32; 3] =
            std::array::from_fn(|i| (0..3).map(|j| covariance[i][j] * axis[j]).sum());
        let length = next.iter().map(|v| v * v).sum::<f32>().sqrt();
        if length < 1e-6 {
            break;
        }
        axis = next.map(|v| v / length);
    }
    let project = |t: &[f32; 3]| (0..3).map(|c| (t[c] - mean[c]) * axis[c]).sum::<f32>();
    let (low, high) = texels
        .iter()
        .map(project)
        .fold((f32::MAX, f32::MIN), |(l, h), p| (l.min(p), h.max(p)));
    let quantize = |p: f32| -> u16 {
        let c: [f32; 3] = std::array::from_fn(|i| (mean[i] + p * axis[i]).clamp(0.0, 255.0));
        ((c[0] * 31.0 / 255.0).round() as u16) << 11
            | ((c[1] * 63.0 / 255.0).round() as u16) << 5
            | (c[2] * 31.0 / 255.0).round() as u16
    };
    let (mut e0, mut e1) = (quantize(high), quantize(low));
    if e0 < e1 {
        std::mem::swap(&mut e0, &mut e1);
    }
    let expand = |e: u16| -> [f32; 3] {
        let (r, g, b) = (e >> 11, e >> 5 & 63, e & 31);
        [
            f32::from(r << 3 | r >> 2),
            f32::from(g << 2 | g >> 4),
            f32::from(b << 3 | b >> 2),
        ]
    };
    let (p0, p1) = (expand(e0), expand(e1));
    let palette: [[f32; 3]; 4] = [
        p0,
        p1,
        std::array::from_fn(|c| (2.0 * p0[c] + p1[c]) / 3.0),
        std::array::from_fn(|c| (p0[c] + 2.0 * p1[c]) / 3.0),
    ];
    let mut indices = 0u32;
    if e0 != e1 {
        for (i, t) in texels.iter().enumerate() {
            let distance = |p: &[f32; 3]| (0..3).map(|c| (p[c] - t[c]).powi(2)).sum::<f32>();
            let nearest = (0..4)
                .min_by(|&a, &b| distance(&palette[a]).total_cmp(&distance(&palette[b])))
                .unwrap_or(0);
            indices |= (nearest as u32) << (2 * i);
        }
    }
    let mut block = [0u8; 8];
    block[..2].copy_from_slice(&e0.to_le_bytes());
    block[2..4].copy_from_slice(&e1.to_le_bytes());
    block[4..].copy_from_slice(&indices.to_le_bytes());
    block
}

/// One mip level of the albedo plate (BC1 sRGB, `edge` texels square) with the lattice's
/// change added where the gearstack level (`stack`, `stack_edge` square) says.
fn bake_level(
    albedo: &[u8],
    edge: usize,
    stack: &[[u8; 4]],
    stack_edge: usize,
    lattice: &Lattice,
) -> Vec<u8> {
    let columns = (edge / 4).max(1);
    let mut out = albedo.to_vec();
    for (index, block) in albedo.chunks_exact(8).enumerate() {
        let mut pixels = [0u8; 64];
        bcdec_rs::bc1(block, &mut pixels, 16);
        let (bx, by) = (index % columns * 4, index / columns * 4);
        let mut texels = [[0f32; 3]; 16];
        let mut changed = false;
        for (i, texel) in texels.iter_mut().enumerate() {
            let (x, y) = ((bx + i % 4).min(edge - 1), (by + i / 4).min(edge - 1));
            let s = stack[(y * stack_edge / edge) * stack_edge + x * stack_edge / edge];
            let base: [f32; 3] = std::array::from_fn(|c| linear(pixels[i * 4 + c]));
            let stack = [s[0], s[1], s[2]].map(|v| f32::from(v) / 255.0);
            let change = lattice.change(stack, base);
            *texel = std::array::from_fn(|c| {
                let value = f32::from(encoded(base[c] + change[c]));
                changed |= (value - f32::from(pixels[i * 4 + c])).abs() >= 1.0;
                value
            });
        }
        if changed {
            out[index * 8..index * 8 + 8].copy_from_slice(&encode_bc1(&texels));
        }
    }
    out
}

impl Assembly<'_> {
    /// The mean linear colour of a source texture's top level, every face of a cube.
    fn texture_mean(&mut self, tag: u32) -> Result<[f32; 4]> {
        let texture = self
            .source
            .linear_texture(tag, self.options.xg_library.as_deref())?;
        let (width, height) = (usize::from(texture.width), usize::from(texture.height));
        let (_, size) = super::super::texture::surface(texture.format, width, height)?;
        let srgb = matches!(texture.format, 29 | 72 | 75 | 78 | 99);
        let mut sum = [0f64; 4];
        let mut count = 0f64;
        for face in 0..usize::from(texture.layers) {
            let data = texture
                .bytes
                .get(face * size..(face + 1) * size)
                .context("Source texture top level is truncated")?
                .to_vec();
            let rgba = crate::tiger::icon::decode(&crate::tiger::icon::Layer {
                slot: 0,
                texture: tag,
                format: texture.format,
                width: texture.width,
                height: texture.height,
                data,
            })?;
            for texel in rgba.chunks_exact(4) {
                for c in 0..4 {
                    let value = if srgb && c < 3 {
                        linear(texel[c])
                    } else {
                        f32::from(texel[c]) / 255.0
                    };
                    sum[c] += f64::from(value);
                }
                count += 1.0;
            }
        }
        Ok(sum.map(|s| (s / count) as f32))
    }

    /// The albedo plate a PS4 surface draw binds in place of `equipment`'s, with the source
    /// program's environment reflection baked in, or `None` when the program adds none. A
    /// program that cannot run on the CPU keeps the plate and records why.
    pub(super) fn reflection_plate(
        &mut self,
        tag: u32,
        draw: &Draw,
        equipment: &Equipment,
    ) -> Result<Option<String>> {
        if draw.stage != 0 || self.source.platform() != 7 || equipment.plates[0].is_empty() {
            return Ok(None);
        }
        match self.measure_reflection(tag, draw, equipment) {
            Ok(plate) => Ok(plate),
            Err(error) => {
                crate::cancellation::check()?;
                self.evidence.push(
                    json!({"material":tag,"environment_reflection_error":format!("{error:#}")}),
                );
                Ok(None)
            }
        }
    }

    fn measure_reflection(
        &mut self,
        tag: u32,
        draw: &Draw,
        equipment: &Equipment,
    ) -> Result<Option<String>> {
        let source = self.source.read(tag)?;
        let pixel = source.u32(0x2A8)?;
        let (rows, slots) = self.pixel_reads(&source, draw, equipment)?;
        if !slots.values().any(|slot| matches!(slot, Slot::Cube(_))) {
            return Ok(None);
        }
        ensure!(
            slots.values().any(|slot| matches!(slot, Slot::Plate(0)))
                && slots.values().any(|slot| matches!(slot, Slot::Plate(2))),
            "Source surface program reads no albedo or gearstack plate"
        );
        let mut means = BTreeMap::new();
        for slot in slots.values() {
            if let Slot::Texture(texture) | Slot::Cube(texture) = *slot
                && !means.contains_key(&texture)
            {
                let mean = self.texture_mean(texture)?;
                means.insert(texture, mean);
            }
        }
        let program = crate::d1_roi::gcn::pixel_program(self.source, pixel)?;
        let level = |i: usize, steps: usize| i as f32 / (steps - 1) as f32;
        let mut lattice = Vec::with_capacity(STEPS.iter().product());
        let mut peak = 0f32;
        for r in 0..STEPS[0] {
            crate::cancellation::check()?;
            for b in 0..STEPS[1] {
                for g in 0..STEPS[2] {
                    for t in 0..STEPS[3] {
                        let mut probe = Probe {
                            rows: &rows,
                            slots: &slots,
                            means: &means,
                            stack: [level(r, STEPS[0]), level(g, STEPS[2]), level(b, STEPS[1])],
                            albedo: level(t, STEPS[3]),
                            cube: true,
                        };
                        let lit = program.run(&probe)?.and_then(|targets| targets[0]);
                        probe.cube = false;
                        let dark = program.run(&probe)?.and_then(|targets| targets[0]);
                        let change: [f32; 3] = match (lit, dark) {
                            (Some(lit), Some(dark)) => std::array::from_fn(|c| lit[c] - dark[c]),
                            _ => [0.0; 3],
                        };
                        ensure!(
                            change.iter().all(|v| v.is_finite()),
                            "Source surface program writes a non-finite albedo"
                        );
                        peak = change.iter().fold(peak, |p, v| p.max(v.abs()));
                        lattice.push(change);
                    }
                }
            }
        }
        if peak < VISIBLE {
            return Ok(None);
        }
        let lattice = Lattice(lattice);
        let (albedo, albedo_edge) = self
            .plate_blocks
            .get(&equipment.plates[0])
            .cloned()
            .context("Source albedo plate blocks are not retained")?;
        let (stack, stack_edge) = self
            .plate_blocks
            .get(&equipment.plates[2])
            .cloned()
            .context("Source gearstack plate blocks are not retained")?;
        // One plate per albedo plate and reflection.
        let mut hash = 0xCBF29CE484222325u64;
        for byte in equipment.plates[0].bytes().chain(
            lattice
                .0
                .iter()
                .flatten()
                .flat_map(|v| ((v * 4096.0).round() as i32).to_le_bytes()),
        ) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001B3);
        }
        let symbol = format!("{}-reflection-{hash:016X}", equipment.plates[0]);
        if !self.symbols.contains(&symbol) {
            let (mut bytes, mut from, mut stack_from) = (Vec::new(), 0, 0);
            for level in 0..5 {
                crate::cancellation::check()?;
                let (edge, other) = ((albedo_edge >> level).max(4), (stack_edge >> level).max(4));
                let size = (edge / 4).pow(2) * 8;
                let stack_size = (other / 4).pow(2) * 8;
                let stack_level = decode_bc1(
                    stack
                        .get(stack_from..stack_from + stack_size)
                        .context("Source gearstack plate level is truncated")?,
                    other,
                );
                bytes.extend(bake_level(
                    albedo
                        .get(from..from + size)
                        .context("Source albedo plate level is truncated")?,
                    edge,
                    &stack_level,
                    other,
                    &lattice,
                ));
                from += size;
                stack_from += stack_size;
            }
            self.texture_bytes(
                &symbol,
                super::super::texture::Texture {
                    format: 72,
                    width: albedo_edge as u16,
                    height: albedo_edge as u16,
                    depth: 1,
                    layers: 1,
                    mips: 5,
                    bytes,
                },
            )?;
        }
        self.evidence.push(json!({"material":tag,"pixel":pixel,"environment_reflection":{
            "plate":symbol,"source_plate":equipment.plates[0],"peak":peak,
            "cube_means":means.iter().filter(|(t, _)| slots.values().any(|s| matches!(s, Slot::Cube(c) if c == *t))).collect::<BTreeMap<_, _>>()}}));
        self.approximations.insert(
            "PS4 gear surfaces add the environment reflection their source program bakes into the albedo, measured with the cube map's mean colour, to a private albedo plate: it follows the gearstack masks but not the view".to_owned(),
        );
        Ok(Some(symbol))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_round_trip_keeps_a_gradient() {
        let texels: [[f32; 3]; 16] =
            std::array::from_fn(|i| [i as f32 * 16.0, 255.0 - i as f32 * 16.0, 128.0]);
        let block = encode_bc1(&texels);
        let mut pixels = [0u8; 64];
        bcdec_rs::bc1(&block, &mut pixels, 16);
        // Four colours across a 240 span: within a sixth of it plus quantization, and the
        // extremes on the endpoints.
        for (i, texel) in texels.iter().enumerate() {
            let limit = if i == 0 || i == 15 { 8.0 } else { 44.0 };
            for c in 0..3 {
                assert!(
                    (f32::from(pixels[i * 4 + c]) - texel[c]).abs() <= limit,
                    "texel {i} lane {c}: {} vs {}",
                    pixels[i * 4 + c],
                    texel[c]
                );
            }
        }
    }

    #[test]
    fn lattice_interpolates_red_and_blue() {
        let mut points = vec![[0f32; 3]; STEPS.iter().product()];
        for r in 0..STEPS[0] {
            for b in 0..STEPS[1] {
                for g in 0..STEPS[2] {
                    for t in 0..STEPS[3] {
                        let v = (1.0 - r as f32 / 16.0) * (b as f32 / 16.0);
                        points[Lattice::index(r, b, g, t)] = [v; 3];
                    }
                }
            }
        }
        let change = Lattice(points).change([0.25, 0.0, 0.5], [0.3; 3]);
        assert!((change[0] - 0.75 * 0.5).abs() < 1e-5);
    }
}
