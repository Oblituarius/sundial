//! PS4 GCN Morton-tiled texture detile via swizzleinator.
use anyhow::{Result, ensure};
use swizzleinator::{
    format::{DxgiFormat, GcnSurfaceFormat},
    swizzle::{Deswizzler, ps::Ps4},
};

/// GCN T# surface format (bits [25:20] of dword 1) → DXGI format number.
///
/// Only the subset parhelion's surface() helper handles is listed; any format
/// not in this table is rejected as unsupported.
pub(super) fn gcn_to_dxgi(gcn: GcnSurfaceFormat) -> Option<u32> {
    Some(match gcn {
        GcnSurfaceFormat::Format8_8_8_8 => 28,  // R8G8B8A8_UNORM
        GcnSurfaceFormat::BC1 => 71,             // BC1_UNORM
        GcnSurfaceFormat::BC3 => 77,             // BC3_UNORM
        GcnSurfaceFormat::BC4 => 80,             // BC4_UNORM
        GcnSurfaceFormat::BC5 => 83,             // BC5_UNORM
        GcnSurfaceFormat::BC6 => 95,             // BC6H_UF16
        GcnSurfaceFormat::BC7 => 98,             // BC7_UNORM
        _ => return None,
    })
}

/// Extract the GCN surface format from a PS4 texture descriptor (T#) header.
///
/// The T# is the first 32 bytes of the header. Bits [25:20] of dword 1 hold
/// the DATA_FORMAT / surface format field.
pub(super) fn gcn_format(hdr: &[u8]) -> Result<GcnSurfaceFormat> {
    ensure!(hdr.len() >= 8, "PS4 texture header too short for T#");
    let dword1 = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
    let raw = ((dword1 >> 20) & 0x3F) as u16;
    GcnSurfaceFormat::try_from(raw).map_err(|_| anyhow::anyhow!("Unknown PS4 GCN format {raw:#X}"))
}

/// Deswizzle one mip surface from a PS4 texture.
///
/// `src` must be exactly the Morton-tiled bytes for this mip.
/// Returns the linear destination bytes.
pub(super) fn deswizzle_mip(
    src: &[u8],
    width: usize,
    height: usize,
    fmt: DxgiFormat,
) -> Result<Vec<u8>> {
    let (_, surface_bytes) = fmt.calculate_pitch(width, height);
    ensure!(
        surface_bytes > 0,
        "PS4 deswizzle: zero-size surface ({width}x{height})"
    );
    let mut src_owned = src.to_vec();
    let mut dest = vec![0u8; surface_bytes];
    // PS4 GCN uses 8x8 Morton macro-tiles; no pow2 source padding for D1 ROI data.
    Ps4::deswizzle(&mut src_owned, &mut dest, (width, height, 1), fmt, false)
        .map_err(|e| anyhow::anyhow!("PS4 deswizzle error: {e:?}"))?;
    Ok(dest)
}

/// Deswizzle the full mip chain (all mips concatenated, mip 0 first) for a PS4
/// texture.  Returns the linearised bytes in the same mip-major order.
pub(super) fn deswizzle_chain(
    raw: &[u8],
    width: u16,
    height: u16,
    mips: u8,
    fmt: DxgiFormat,
) -> Result<Vec<u8>> {
    ensure!(mips > 0 && mips <= 16, "Invalid PS4 mip count {mips}");
    let mut result = Vec::new();
    let mut offset = 0usize;
    for mip in 0..mips {
        let w = usize::from(width >> mip).max(1);
        let h = usize::from(height >> mip).max(1);
        let (_, mip_bytes) = fmt.calculate_pitch(w, h);
        ensure!(
            offset + mip_bytes <= raw.len(),
            "PS4 texture data truncated at mip {mip} (need {mip_bytes}, have {})",
            raw.len() - offset
        );
        let linear = deswizzle_mip(&raw[offset..offset + mip_bytes], w, h, fmt)?;
        result.extend_from_slice(&linear);
        offset += mip_bytes;
    }
    Ok(result)
}

/// Map a parhelion DXGI format number to the swizzleinator `DxgiFormat` used
/// when calling the PS4 detiler.
pub(super) fn dxgi_fmt(format: u32) -> Result<DxgiFormat> {
    DxgiFormat::try_from(format).map_err(|_| anyhow::anyhow!("Unsupported PS4 DXGI format {format}"))
}
