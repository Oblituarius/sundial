//! Translate D1 PS4 GCN programs into D1-equivalent SM5 programs.
//!
//! The PS4 build ships each program as GCN machine code. The Xbox One build of the same
//! program is SM5 bytecode declaring the same API registers: material constants in `cb0`,
//! bones in `cb10`, model rows in `cb11`, view rows in `cb12`, and textures and samplers by
//! the slots the material binds. The Gnm input usage slots in each program header name
//! those API registers, so a GCN program lifted to HLSL and compiled declares what its Xbox
//! twin declares, and the Xbox path adapts it to Shadowkeep unchanged.
//!
//! Program header (tag types 8 and 9, the code in the entry's reference):
//! - pixel: `+0x00` code size (low 24 bits) and input usage slot count (high byte),
//!   `+0x10` SPI registers (`+0x24` SPI_PS_INPUT_ADDR), `+0x38` interpolant count, slots
//!   at `+0x3C`, then 2-byte `GnmPixelInputSemantic`s.
//! - vertex: 0x14 bytes, then the same shader word at `+0x14`; `+0x38` vertex input and
//!   export counts, slots at `+0x3C`, 4-byte `GnmVertexInputSemantic`s, then 2-byte
//!   `GnmVertexExportSemantic`s.
//!
//! Slots are freegnm's `GnmInputUsageSlot` (`include/gnm/shader.h`). Verified on all 1,885
//! pixel and 941 vertex programs of the gear effect scope.
use super::*;

mod decode;
mod lift;

pub(super) struct Slot {
    pub kind: u8,
    pub api: u8,
    pub register: u8,
    pub flags: u8,
}

pub(super) enum Interface {
    /// Vertex inputs (layout element, first VGPR, lanes) and exports (semantic, parameter).
    Vertex {
        inputs: Vec<(u8, u8, u8)>,
        exports: Vec<(u8, u8)>,
    },
    /// SPI_PS_INPUT_ADDR and the interpolants' `GnmPixelInputSemantic`s by attribute.
    Pixel { input_addr: u32, inputs: Vec<u16> },
}

pub(super) struct Header {
    pub slots: Vec<Slot>,
    pub interface: Interface,
}

/// A source program's header and GCN code.
fn program(source: &Source, tag: u32, vertex: bool) -> Result<(Header, Vec<u8>)> {
    let header = source.read(tag)?;
    let code = source.read(source.reference(tag)?)?;
    parse(&header, &code.0, vertex).with_context(|| format!("Source program {tag:08X}"))
}

fn parse(d: &Payload, code: &[u8], vertex: bool) -> Result<(Header, Vec<u8>)> {
    let base = if vertex { 0x14 } else { 0 };
    let word = d.u32(base)?;
    let (size, count) = ((word & 0xFF_FFFF) as usize, (word >> 24) as usize);
    let mut at = 0x3C;
    let mut slots = Vec::with_capacity(count);
    for _ in 0..count {
        slots.push(Slot {
            kind: d.u8(at)?,
            api: d.u8(at + 1)?,
            register: d.u8(at + 2)?,
            flags: d.u8(at + 3)?,
        });
        at += 4;
    }
    let interface = if vertex {
        ensure!(
            d.u32(4)? as usize == size,
            "Source vertex program header differs"
        );
        let (inputs, exports) = (usize::from(d.u8(0x38)?), usize::from(d.u8(0x39)?));
        let mut list = Vec::new();
        for _ in 0..inputs {
            list.push((d.u8(at)?, d.u8(at + 1)?, d.u8(at + 2)?));
            at += 4;
        }
        let mut params = Vec::new();
        for _ in 0..exports {
            let flags = d.u8(at + 1)?;
            ensure!(flags >> 6 == 0, "Source vertex program exports half floats");
            params.push((d.u8(at)?, flags & 0x1F));
            at += 2;
        }
        Interface::Vertex {
            inputs: list,
            exports: params,
        }
    } else {
        let count = d.u32(0x38)? as usize;
        let mut inputs = Vec::new();
        for _ in 0..count {
            inputs.push(d.u16(at)?);
            at += 2;
        }
        Interface::Pixel {
            input_addr: d.u32(0x24)?,
            inputs,
        }
    };
    ensure!(size <= code.len(), "Source program code is truncated");
    Ok((Header { slots, interface }, code[..size].to_vec()))
}

/// Compiled SM5 programs for one material's vertex and pixel stages, and their HLSL.
pub(super) struct Lifted {
    pub vertex: Vec<u8>,
    pub pixel: Vec<u8>,
    pub hlsl: [String; 2],
}

/// Lift a material's vertex and pixel programs. `elements` are the draw's native layout
/// elements, (semantic, index) in stream order: element k feeds vertex input k, as the
/// prebuilt fetch shaders load one buffer descriptor per element. `cubes` are the pixel
/// texture slots the material binds cube maps to.
pub(super) fn lift(
    source: &Source,
    vertex: u32,
    pixel: u32,
    elements: &[(u8, u8)],
    cubes: &std::collections::BTreeSet<u8>,
) -> Result<Lifted> {
    let (vertex_header, vertex_code) = program(source, vertex, true)?;
    let (pixel_header, pixel_code) = program(source, pixel, false)?;
    let Interface::Vertex { exports, .. } = &vertex_header.interface else {
        unreachable!()
    };
    let mut linkage = exports.clone();
    linkage.sort_unstable_by_key(|(_, param)| *param);
    let linkage = linkage.into_iter().map(|(semantic, _)| semantic).collect::<Vec<_>>();
    let vertex_hlsl = lift::hlsl(&vertex_header, &vertex_code, elements, &[], &Default::default())
        .with_context(|| format!("Source vertex program {vertex:08X}"))?;
    let pixel_hlsl = lift::hlsl(&pixel_header, &pixel_code, &[], &linkage, cubes)
        .with_context(|| format!("Source pixel program {pixel:08X}"))?;
    let (vertex_code, _) = crate::tiger::shader::compile(&vertex_hlsl, true)
        .with_context(|| format!("Lifted vertex program {vertex:08X}"))?;
    let (pixel_code, _) = crate::tiger::shader::compile(&pixel_hlsl, false)
        .with_context(|| format!("Lifted pixel program {pixel:08X}"))?;
    Ok(Lifted {
        vertex: vertex_code,
        pixel: pixel_code,
        hlsl: [vertex_hlsl, pixel_hlsl],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Lift and compile every program pair a `fxdump` survey wrote (`GCN_FX_DIR`, with
    /// `materials.txt` and `ps/`, `vs/` header and code files), and report failures by reason.
    #[test]
    #[ignore]
    fn lift_surveyed_effect_programs() {
        let Ok(dir) = std::env::var("GCN_FX_DIR") else { return };
        let dir = std::path::Path::new(&dir);
        let read = |kind: &str, tag: &str| -> (Payload, Vec<u8>) {
            let base = dir.join(kind).join(tag);
            (
                Payload(std::fs::read(base.with_extension("hdr")).unwrap()),
                std::fs::read(base.with_extension("gcn")).unwrap(),
            )
        };
        let mut pairs = BTreeSet::new();
        for line in std::fs::read_to_string(dir.join("materials.txt")).unwrap().lines() {
            let words = line.split_whitespace().collect::<Vec<_>>();
            if words.len() == 5 && words[4] != "FFFFFFFF" && words[2] != "FFFFFFFF" {
                pairs.insert((words[2].to_owned(), words[4].to_owned()));
            }
        }
        let only = std::env::var("GCN_FX_ONLY").ok();
        let mut reasons = BTreeMap::<String, usize>::new();
        let (mut lifted, mut compiled) = (0, 0);
        let mut pixels = BTreeSet::new();
        for (vs, ps) in &pairs {
            if only.as_ref().is_some_and(|only| only != ps) || !pixels.insert(ps.clone()) {
                continue;
            }
            let outcome = (|| -> Result<()> {
                let (vh, vc) = read("vs", vs);
                let (ph, pc) = read("ps", ps);
                let (vh, vc) = parse(&vh, &vc, true)?;
                let (ph, pc) = parse(&ph, &pc, false)?;
                let Interface::Vertex { exports, inputs } = &vh.interface else { unreachable!() };
                let mut params = exports.clone();
                params.sort_unstable_by_key(|(_, p)| *p);
                let linkage = params.iter().map(|(s, _)| *s).collect::<Vec<_>>();
                let elements = (0..=inputs.iter().map(|i| i.0).max().unwrap_or(0))
                    .map(|k| if k == 0 { (0, 0) } else { (5, k) })
                    .collect::<Vec<_>>();
                let v = lift::hlsl(&vh, &vc, &elements, &[], &BTreeSet::new()).context("vertex")?;
                // The survey keeps no texture types, so cube maps lift as 2D textures here.
                let p = lift::hlsl(&ph, &pc, &[], &linkage, &BTreeSet::new()).context("pixel")?;
                if only.is_some() {
                    println!("{v}\n----\n{p}");
                }
                lifted += 1;
                crate::tiger::shader::compile(&v, true).context("compile vertex")?;
                crate::tiger::shader::compile(&p, false).context("compile pixel")?;
                compiled += 1;
                Ok(())
            })();
            if let Err(error) = outcome {
                let text = format!("{error:#}");
                let key = text.split(": ").take(2).collect::<Vec<_>>().join(": ");
                let key = key.chars().take(120).collect::<String>();
                *reasons.entry(key).or_default() += 1;
            }
        }
        println!("pixel programs {} lifted {lifted} compiled {compiled}", pixels.len());
        let mut reasons = reasons.into_iter().collect::<Vec<_>>();
        reasons.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (reason, n) in reasons.iter().take(40) {
            println!("{n:5} {reason}");
        }
    }
}
