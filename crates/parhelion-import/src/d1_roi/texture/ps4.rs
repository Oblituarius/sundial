//! PS4 GCN texture descriptors and 1D-thin (8x8 Morton micro-tile) detiling.
use anyhow::{Result, bail, ensure};
use swizzleinator::{
    format::DxgiFormat,
    swizzle::{Deswizzler, ps::Ps4},
};

const TILE_THIN_1D: u32 = 13;

/// Fields of the 32-byte GCN T# at the start of a PS4 texture header.
pub(super) struct Descriptor {
    data_format: u32,
    num_format: u32,
    tiling: u32,
    pow2pad: bool,
    kind: u32,
}

pub(super) fn descriptor(header: &[u8]) -> Result<Descriptor> {
    ensure!(header.len() >= 0x20, "PS4 texture header too short for T#");
    let dword = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
    let (d1, d3) = (dword(1), dword(3));
    Ok(Descriptor {
        data_format: (d1 >> 20) & 0x3F,
        num_format: (d1 >> 26) & 0xF,
        tiling: (d3 >> 20) & 0x1F,
        pow2pad: (d3 >> 25) & 1 == 1,
        kind: (d3 >> 28) & 0xF,
    })
}

impl Descriptor {
    /// DXGI number for the formats the native writer accepts. The channel type (num format,
    /// 9 = SRGB) decides the sRGB variant.
    pub(super) fn dxgi(&self) -> Result<u32> {
        Ok(match (self.data_format, self.num_format) {
            (10, 0) => 28,
            (10, 9) => 29,
            (35, 0) => 71,
            (35, 9) => 72,
            (37, 0) => 77,
            (37, 9) => 78,
            (38, 0) => 80,
            (38, 1) => 81,
            (39, 0) => 83,
            (39, 1) => 84,
            (41, 0) => 98,
            (41, 9) => 99,
            (format, channel) => {
                bail!("Unsupported PS4 texture format {format} with channel type {channel}")
            }
        })
    }

    pub(super) fn is_cube(&self) -> bool {
        self.kind == 11
    }
}

/// Linear mip chain (mip 0 first). Tiled storage pads each mip to a power of two when the T#
/// requests it, then to whole 8x8 micro tiles of texels or blocks. Some textures, such as
/// plate sources, are stored linear although their T# names a tile mode; the storage size
/// decides, and `linear_hint` (Charm's rule) breaks a tie when both layouts have one size.
pub(super) fn linear_chain(
    raw: &[u8],
    descriptor: &Descriptor,
    format: u32,
    width: u16,
    height: u16,
    mips: u8,
    linear_hint: bool,
) -> Result<Vec<u8>> {
    ensure!(mips > 0 && mips <= 16, "Invalid PS4 mip count {mips}");
    let dxgi = DxgiFormat::try_from(format)
        .map_err(|_| anyhow::anyhow!("Unsupported PS4 DXGI format {format}"))?;
    let edge = if format >= 70 { 4 } else { 1 };
    let levels = (0..mips)
        .map(|mip| {
            let w = usize::from(width >> mip).max(1);
            let h = usize::from(height >> mip).max(1);
            let (pw, ph) = if descriptor.pow2pad {
                (w.next_power_of_two(), h.next_power_of_two())
            } else {
                (w, h)
            };
            let tiled_w = pw.div_ceil(edge).div_ceil(8) * 8 * edge;
            let tiled_h = ph.div_ceil(edge).div_ceil(8) * 8 * edge;
            let (_, linear) = super::surface(format, w, h)?;
            Ok((
                w,
                h,
                tiled_w,
                tiled_h,
                dxgi.calculate_pitch(tiled_w, tiled_h).1,
                linear,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let tiled_total = levels.iter().map(|l| l.4).sum::<usize>();
    let linear_total = levels.iter().map(|l| l.5).sum::<usize>();
    let linear = match (raw.len() == tiled_total, raw.len() == linear_total) {
        (true, true) => linear_hint,
        (true, false) => false,
        (false, true) => true,
        _ => anyhow::bail!(
            "PS4 texture storage of {} bytes is neither tiled ({tiled_total}) nor linear ({linear_total})",
            raw.len()
        ),
    };
    if linear {
        return Ok(raw.to_vec());
    }
    ensure!(
        descriptor.tiling == TILE_THIN_1D,
        "Unsupported PS4 texture tile mode {}",
        descriptor.tiling
    );
    let mut result = Vec::with_capacity(linear_total);
    let mut offset = 0;
    for (w, h, tiled_w, tiled_h, tiled_bytes, _) in levels {
        let mut source = raw[offset..offset + tiled_bytes].to_vec();
        let mut padded = vec![0; tiled_bytes];
        Ps4::deswizzle(&mut source, &mut padded, (tiled_w, tiled_h, 1), dxgi, false)
            .map_err(|e| anyhow::anyhow!("PS4 detile failed: {e:?}"))?;
        let (padded_row, _) = super::surface(format, tiled_w, tiled_h)?;
        let (row, length) = super::surface(format, w, h)?;
        for line in 0..length / row {
            let at = line * padded_row;
            result.extend_from_slice(&padded[at..at + row]);
        }
        offset += tiled_bytes;
    }
    Ok(result)
}
