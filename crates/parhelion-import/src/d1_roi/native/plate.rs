//! Preserve the source runtime's declared canvases and five BC mip levels.
use super::super::texture::{Texture, surface};
use super::material::Equipment;
use super::*;

/// D1 gearstack BC1 blocks rewritten in the stock gear convention as BC3. D1 gear pixel
/// programs (such as `80A0881C`) read red, a dye and detail mask, and green, the
/// smoothness; D1 decal programs (such as `80A39D51`) read red and blue, the decal
/// coverage. Stock gear program `81532DEA` reads ambient occlusion from red, smoothness
/// from green and dyes where alpha is at least 40/255 (smaller alphas are emissive codes).
/// Stock decal program `80C1CB29` reads its coverage as blue times 255/32. D1 has no
/// per-texel occlusion or emission, so red is one, alpha is a dye selector at the D1
/// mask's midpoint, and blue is scaled down by 255/32. BC3 colour blocks always use four
/// colours, so a three-colour source block is refitted.
fn native_gear_stack(blocks: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        blocks.len().is_multiple_of(8),
        "Source gearstack plate is not whole BC1 blocks"
    );
    let expand = |value: u16, bits: u32| {
        let value = u32::from(value);
        (value << (8 - bits)) | (value >> (2 * bits - 8))
    };
    let mut out = Vec::with_capacity(blocks.len() * 2);
    for block in blocks.chunks_exact(8) {
        let ends = [
            u16::from_le_bytes([block[0], block[1]]),
            u16::from_le_bytes([block[2], block[3]]),
        ];
        let four = ends[0] > ends[1];
        let channel = |shift: u16, bits: u32| {
            let [e0, e1] = ends.map(|c| expand((c >> shift) & ((1 << bits) - 1), bits));
            if four {
                [e0, e1, (2 * e0 + e1) / 3, (e0 + 2 * e1) / 3]
            } else {
                [e0, e1, (e0 + e1) / 2, 0]
            }
        };
        let (red, green, blue) = (channel(11, 5), channel(5, 6), channel(0, 5));
        let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
        let texel = |i: u32| ((indices >> (2 * i)) & 3) as usize;
        let mut alpha = 0u64;
        for i in 0..16 {
            alpha |= u64::from(red[texel(i)] < 128) << (3 * i);
        }
        out.extend([255, 0]);
        out.extend(&alpha.to_le_bytes()[..6]);
        if four {
            // The same endpoints and indices interpolate green and the scaled blue exactly.
            for end in ends {
                let coverage = (u32::from(end & 31) * 32 + 127) / 255;
                out.extend((0xF800 | (end & 0x07E0) | coverage as u16).to_le_bytes());
            }
            out.extend(indices.to_le_bytes());
            continue;
        }
        let texels = (0..16)
            .map(|i| [green[texel(i)], blue[texel(i)] * 32 / 255])
            .collect::<Vec<_>>();
        let distance =
            |a: [u32; 2], b: [u32; 2]| a[0].abs_diff(b[0]).pow(2) + a[1].abs_diff(b[1]).pow(2);
        let (mut far, mut span) = ((0, 0), 0);
        for a in 0..16 {
            for b in a + 1..16 {
                if distance(texels[a], texels[b]) > span {
                    (far, span) = ((a, b), distance(texels[a], texels[b]));
                }
            }
        }
        let quantize = |[g, b]: [u32; 2]| {
            0xF800 | (((g * 63 + 127) / 255) as u16) << 5 | ((b * 31 + 127) / 255) as u16
        };
        let (e0, e1) = (quantize(texels[far.0]), quantize(texels[far.1]));
        let [q0, q1] = [e0, e1].map(|e| [expand((e >> 5) & 63, 6), expand(e & 31, 5)]);
        let mix = |a: u32, b: u32| [0, 1].map(|c| (a * q0[c] + b * q1[c]) / 3);
        let palette = [q0, q1, mix(2, 1), mix(1, 2)];
        let mut selected = 0u32;
        for (i, value) in texels.iter().enumerate() {
            let nearest = (0..4)
                .min_by_key(|&index| distance(palette[index], *value))
                .unwrap_or(0);
            selected |= (nearest as u32) << (2 * i);
        }
        out.extend(e0.to_le_bytes());
        out.extend(e1.to_le_bytes());
        out.extend(selected.to_le_bytes());
    }
    Ok(out)
}

impl Assembly<'_> {
    fn plate(&mut self, tag: u32) -> Result<String> {
        let symbol = format!("plate-{tag:08X}");
        if self.symbols.contains(&symbol) {
            return Ok(symbol);
        }
        ensure!(
            self.source.reference(tag)? == 0x80800147,
            "Source plate class differs"
        );
        let plate = self.source.read(tag)?;
        let kind = usize::from(plate.u16(8)?);
        let canvas = usize::from(plate.u16(10)?);
        // Xbox build 56578: 140248C40 indexes 1411F46F0 by equipment class
        // and canvas. Class 0 is player armor and class 1 is weapon.
        let canvases: &[usize] = if self.weapon {
            &[2048, 1024, 512]
        } else {
            &[2048, 2048, 1024, 1024]
        };
        let edge = *canvases
            .get(canvas)
            .context("Unknown canvas for the source equipment class")?;
        let format = *[72u32, 83, 71]
            .get(kind)
            .context("Unknown source plate channel")?;
        // 14089CD00 copies BC blocks without filtering, using these exact fallbacks.
        let fallback: &[u8] = match kind {
            0 => &[0x10, 0x84, 0x10, 0x84, 0, 0, 0, 0],
            1 => &[0x80, 0x7F, 0, 0, 0, 0, 0, 0, 0x80, 0x7F, 0, 0, 0, 0, 0, 0],
            2 => &[0xFF, 0xFF, 0, 0, 0, 0, 0, 0],
            _ => unreachable!(),
        };
        let block = fallback.len();
        let mut offsets = Vec::new();
        let mut bytes = Vec::new();
        for level in 0..5 {
            offsets.push(bytes.len());
            bytes.extend(fallback.repeat((edge >> (level + 2)).pow(2)));
        }
        let rows = plate.array(16, 20, Some(0x80800163))?;
        ensure!(!rows.is_empty(), "Required source plate is empty");
        let mut placements = Vec::new();
        for row in rows {
            let source = plate.u32(row)?;
            let (x, y, w, h) = (
                plate.u32(row + 4)? as usize,
                plate.u32(row + 8)? as usize,
                plate.u32(row + 12)? as usize,
                plate.u32(row + 16)? as usize,
            );
            ensure!(
                w > 0
                    && h > 0
                    && x.checked_add(w).is_some_and(|end| end <= edge)
                    && y.checked_add(h).is_some_and(|end| end <= edge),
                "Source plate placement exceeds its declared canvas"
            );
            let texture = self
                .source
                .linear_texture(source, self.options.xg_library.as_deref())?;
            let header = self.source.read(source)?;
            let valid = texture.layers == 1
                && texture.format == format
                && texture.mips == 5
                && usize::from(texture.width) == w
                && usize::from(texture.height) == h
                // Half the block size: Xbox +0x38, PS4 +0x30 (just before its mip count).
                && usize::from(header.u8(self.source.field(0x38, 0x30))?) * 2 == block;
            let mut input = 0;
            for (level, &output) in offsets.iter().enumerate() {
                let (bx, by, bw, bh, pitch) = (
                    x >> (level + 2),
                    y >> (level + 2),
                    w >> (level + 2),
                    h >> (level + 2),
                    edge >> (level + 2),
                );
                for line in 0..bh {
                    let to = output + ((by + line) * pitch + bx) * block;
                    if valid {
                        let from = input + line * bw * block;
                        let row = texture
                            .bytes
                            .get(from..from + bw * block)
                            .context("Source plate mip extent differs")?;
                        bytes[to..to + bw * block].copy_from_slice(row);
                    } else {
                        bytes[to..to + bw * block].copy_from_slice(&fallback.repeat(bw));
                    }
                }
                if valid {
                    input += surface(format, (w >> level).max(1), (h >> level).max(1))?.1;
                }
            }
            placements.push(
                json!({"source":source,"rectangle":[x,y,w,h],"source_runtime_fallback":!valid}),
            );
        }
        self.evidence.push(json!({"plate":tag,"symbol":symbol,"equipment_class":if self.weapon {"weapon"} else {"player"},"canvas":canvas,"edge":edge,"placements":placements,"format":format,"mips":5,"composition":"source_runtime_bc_block_copy","source_executable_build":"56578.17.04.12.1251.evil"}));
        if kind == 2 && self.source.platform() == 7 {
            // PS4 draws borrow stock gear programs, which read the gearstack differently.
            self.texture_bytes(
                &format!("{symbol}-gear"),
                Texture {
                    format: 77,
                    width: edge as u16,
                    height: edge as u16,
                    depth: 1,
                    layers: 1,
                    mips: 5,
                    bytes: native_gear_stack(&bytes)?,
                },
            )?;
        }
        self.texture_bytes(
            &symbol,
            Texture {
                format,
                width: edge as u16,
                height: edge as u16,
                depth: 1,
                layers: 1,
                mips: 5,
                bytes,
            },
        )
    }

    pub(super) fn equipment(&mut self, owner: &Payload, dyes: &[Value]) -> Result<Equipment> {
        let resource = owner.pointer(24)?;
        let stride = self.source.field(56, 0x30);
        let rows = owner.array(
            resource + self.source.field(0x220, 0x1A8),
            stride,
            Some(0x80801B12),
        )?;
        ensure!(
            rows.len() <= 1,
            "Source owner needs multiple plate selection semantics"
        );
        let mut plates = [String::new(), String::new(), String::new()];
        if let Some(row) = rows.first() {
            let set = self.source.read(owner.u32(row + stride - 8)?)?;
            for (index, output) in plates.iter_mut().enumerate() {
                let tag = set.u32(0x24 + index * 4)?;
                if !matches!(tag, 0 | u32::MAX) {
                    *output = self.plate(tag)?;
                }
            }
        }
        let mut selected = BTreeMap::new();
        for dye in dyes {
            let tag = u32::try_from(dye["tag"].as_u64().context("Dye source")?)?;
            // 140884FE0 selects a local dye slot, not the investment channel.
            // Weapon investment channels 5 and 6 declare local slots 0 and 1.
            let mut slot = self.source.read(tag)?.u32(0x10)?;
            let channel = dye["channel"].as_u64().context("Dye channel")?;
            if self.weapon && self.source.platform() == 7 {
                // PS4 weapon dyes all declare slot type 0, so Zhalo Supercell's two locked
                // dyes would share a slot. Channel table 80A5E249 lists Weapon1, Weapon2
                // and Weapon3 at indices 5 to 7, the slots the weapon's draws select.
                ensure!(
                    (5..8).contains(&channel),
                    "Source weapon dye has a non-weapon channel"
                );
                slot = u32::try_from(channel - 5)?;
            }
            ensure!(slot < 4, "Source dye has an unsupported local slot");
            selected.insert(slot as u8, tag);
        }
        let gear_stack = if self.source.platform() == 7 && !plates[2].is_empty() {
            format!("{}-gear", plates[2])
        } else {
            String::new()
        };
        Ok(Equipment {
            plates,
            gear_stack,
            dyes: selected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::native_gear_stack;

    #[test]
    fn gear_stack_moves_the_mask_to_alpha_and_keeps_smoothness() {
        // Four-colour BC1: (31, 63, 0), black, and their thirds in texels 0 to 3.
        let block = [0xE0, 0xFF, 0, 0, 0xE4, 0, 0, 0];
        let out = native_gear_stack(&block).unwrap();
        // Red at or above the midpoint dyes: texels 1 (black) and 3 (a third) do not.
        assert_eq!(out[..8], [255, 0, 0x08, 0x02, 0, 0, 0, 0]);
        // Full occlusion red, the source greens re-quantized, no blue.
        assert_eq!(out[8..], [0xE0, 0xFF, 0x00, 0xF8, 0xE4, 0, 0, 0]);
    }

    #[test]
    fn gear_stack_scales_decal_coverage_into_the_stock_range() {
        // Full blue against black: full coverage becomes 4/31, which the stock decal
        // program reads as 33/255 times 255/32, saturated to one.
        let block = [0x1F, 0, 0, 0, 0xE4, 0, 0, 0];
        let out = native_gear_stack(&block).unwrap();
        assert_eq!(out[8..], [0x04, 0xF8, 0x00, 0xF8, 0xE4, 0, 0, 0]);
    }

    #[test]
    fn gear_stack_refits_three_colour_blocks() {
        // Three-colour mode: green (0, 63, 0) and its half, then the black index 3.
        let block = [0, 0, 0xE0, 0x07, 0xE4, 0, 0, 0];
        let out = native_gear_stack(&block).unwrap();
        let (e0, e1) = (
            u16::from_le_bytes([out[8], out[9]]),
            u16::from_le_bytes([out[10], out[11]]),
        );
        let selected = u32::from_le_bytes([out[12], out[13], out[14], out[15]]);
        let green = |end: u16| (end >> 5) & 63;
        let index = |i: u32| (selected >> (2 * i)) & 3;
        // Texel 1 is full green, texels 0 and 3 black, texel 2 between them.
        let full = if green(e0) == 63 { 0 } else { 1 };
        assert_eq!(green([e0, e1][1 - full as usize]), 0);
        assert_eq!(index(1), full);
        assert_eq!(index(0), 1 - full);
        assert_eq!(index(3), 1 - full);
        assert!(matches!(index(2), 2 | 3));
    }
}
