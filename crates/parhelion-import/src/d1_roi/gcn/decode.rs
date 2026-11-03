//! Decode the GCN (Sea Islands) machine code of D1 PS4 programs.
//!
//! Encodings and opcode numbers follow freegnm (`src/gcn/microcode.h`,
//! `include/gnm/gcn/types.h`). Only the fields the lifter reads are kept.
use anyhow::{Context, Result, bail, ensure};

/// A source operand, with an inline or literal constant already resolved to its bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    Sgpr(u8),
    Vgpr(u8),
    VccLo,
    VccHi,
    M0,
    ExecLo,
    ExecHi,
    Scc,
    Vccz,
    Execz,
    Const(u32),
}

#[derive(Clone, Debug)]
pub enum Op {
    Sop2 { op: u8, dst: u8, src: [Src; 2] },
    Sopk { op: u8, dst: u8, imm: u16 },
    Sop1 { op: u8, dst: u8, src: Src },
    Sopc { op: u8, src: [Src; 2] },
    Sopp { op: u8, imm: u16 },
    Smrd { op: u8, dst: u8, base: u8, offset: SmrdOffset },
    /// VOP1, VOP2, VOPC and VOP3 in the VOP3 opcode space: VOPC 0-255, VOP2 256 + op,
    /// VOP1 384 + op, VOP3-only ops between. `literal` is a VOP2 madak/madmk constant.
    Valu(Valu),
    Vintrp { op: u8, dst: u8, attr: u8, channel: u8 },
    Ds { op: u8 },
    Mtbuf(Buffer),
    Mubuf(Buffer),
    Mimg(Image),
    Exp(Export),
}

#[derive(Clone, Copy, Debug)]
pub enum SmrdOffset {
    Dwords(u32),
    /// A register offset, which the lift does not support.
    Register,
}

#[derive(Clone, Debug)]
pub struct Valu {
    pub op: u16,
    pub dst: u8,
    /// VOP3b carry or compare destination; `None` in the 32-bit encodings, where a
    /// compare and a carry write VCC.
    pub sdst: Option<u8>,
    pub src: Vec<Src>,
    pub abs: u8,
    pub neg: u8,
    pub clamp: bool,
    pub omod: u8,
    pub literal: Option<u32>,
    /// Encoded as VOP3 (64-bit).
    pub vop3: bool,
}

#[derive(Clone, Debug)]
pub struct Buffer {
    pub op: u8,
    pub offset: u16,
    pub offen: bool,
    pub idxen: bool,
    pub dfmt: u8,
    pub nfmt: u8,
    pub vaddr: u8,
    pub vdata: u8,
    pub resource: u8,
    pub soffset: Src,
}

#[derive(Clone, Debug)]
pub struct Image {
    pub op: u8,
    pub dmask: u8,
    pub unorm: bool,
    pub da: bool,
    pub tfe: bool,
    pub lwe: bool,
    pub vaddr: u8,
    pub vdata: u8,
    pub resource: u8,
    pub sampler: u8,
}

#[derive(Clone, Debug)]
pub struct Export {
    pub enable: u8,
    pub target: u8,
    pub compressed: bool,
    pub done: bool,
    pub valid_mask: bool,
    pub src: [u8; 4],
}

#[derive(Clone, Debug)]
pub struct Instruction {
    /// Byte offset in the program.
    pub at: usize,
    pub op: Op,
}

fn word(code: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        code.get(at..at + 4)
            .context("GCN program ends inside an instruction")?
            .try_into()
            .unwrap(),
    ))
}

/// A 9-bit source field; 255 reads the literal dword after the instruction.
fn source(field: u16, literal: &mut Option<u32>, next: Result<u32>) -> Result<Src> {
    Ok(match field {
        0..=103 => Src::Sgpr(field as u8),
        106 => Src::VccLo,
        107 => Src::VccHi,
        124 => Src::M0,
        126 => Src::ExecLo,
        127 => Src::ExecHi,
        128 => Src::Const(0),
        129..=192 => Src::Const(u32::from(field) - 128),
        193..=208 => Src::Const((-(i32::from(field) - 192)) as u32),
        240 => Src::Const(0.5f32.to_bits()),
        241 => Src::Const((-0.5f32).to_bits()),
        242 => Src::Const(1.0f32.to_bits()),
        243 => Src::Const((-1.0f32).to_bits()),
        244 => Src::Const(2.0f32.to_bits()),
        245 => Src::Const((-2.0f32).to_bits()),
        246 => Src::Const(4.0f32.to_bits()),
        247 => Src::Const((-4.0f32).to_bits()),
        251 => Src::Vccz,
        252 => Src::Execz,
        253 => Src::Scc,
        255 => {
            let value = next?;
            *literal = Some(value);
            Src::Const(value)
        }
        256..=511 => Src::Vgpr((field - 256) as u8),
        other => bail!("Unsupported GCN operand {other}"),
    })
}

/// VOP3 opcodes with a scalar carry or compare destination instead of abs and clamp.
fn vop3b(op: u16) -> bool {
    // v_add_i32 .. v_subbrev_u32, v_div_scale_f32/f64.
    (293..=299).contains(&op) || matches!(op, 365 | 366)
}

/// Source count of a VALU opcode in the VOP3 space.
fn valu_sources(op: u16) -> Result<usize> {
    Ok(match op {
        0..=255 => 2,
        // VOP2 with an implicit third operand: v_cndmask (VCC), v_mac (dst), carries.
        256 => 3,
        257..=319 => 2,
        320..=383 => match op {
            // 64-bit shifts and integer multiplies take two sources.
            353..=355 | 357..=364 => 2,
            _ => 3,
        },
        384..=511 => 1,
        other => bail!("Unsupported GCN vector opcode {other}"),
    })
}

/// Decode a whole program, stopping after `s_endpgm`.
pub fn program(code: &[u8]) -> Result<Vec<Instruction>> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        ensure!(at < code.len(), "GCN program has no s_endpgm");
        let (op, size) = instruction(code, at)?;
        let end = matches!(op, Op::Sopp { op: 1, .. });
        out.push(Instruction { at, op });
        at += size;
        if end {
            return Ok(out);
        }
    }
}

fn instruction(code: &[u8], at: usize) -> Result<(Op, usize)> {
    let w = word(code, at)?;
    let next = |offset: usize| word(code, at + offset);
    let mut literal = None;
    // 9-bit scalar formats.
    match w >> 23 {
        0x17D => {
            let src = source((w & 0xFF) as u16, &mut literal, next(4))?;
            let op = Op::Sop1 {
                op: ((w >> 8) & 0xFF) as u8,
                dst: ((w >> 16) & 0x7F) as u8,
                src,
            };
            return Ok((op, 4 + 4 * usize::from(literal.is_some())));
        }
        0x17E => {
            let a = source((w & 0xFF) as u16, &mut literal, next(4))?;
            let b = source(((w >> 8) & 0xFF) as u16, &mut literal, next(4))?;
            let op = Op::Sopc {
                op: ((w >> 16) & 0x7F) as u8,
                src: [a, b],
            };
            return Ok((op, 4 + 4 * usize::from(literal.is_some())));
        }
        0x17F => {
            return Ok((
                Op::Sopp {
                    op: ((w >> 16) & 0x7F) as u8,
                    imm: (w & 0xFFFF) as u16,
                },
                4,
            ));
        }
        _ => {}
    }
    match w >> 25 {
        0x3F => {
            let src = source((w & 0x1FF) as u16, &mut literal, next(4))?;
            let op = ((w >> 9) & 0xFF) as u16;
            return Ok((
                Op::Valu(Valu {
                    op: 384 + op,
                    dst: ((w >> 17) & 0xFF) as u8,
                    sdst: None,
                    src: vec![src],
                    abs: 0,
                    neg: 0,
                    clamp: false,
                    omod: 0,
                    literal,
                    vop3: false,
                }),
                4 + 4 * usize::from(literal.is_some()),
            ));
        }
        0x3E => {
            let a = source((w & 0x1FF) as u16, &mut literal, next(4))?;
            let b = Src::Vgpr(((w >> 9) & 0xFF) as u8);
            return Ok((
                Op::Valu(Valu {
                    op: ((w >> 17) & 0xFF) as u16,
                    dst: 0,
                    sdst: None,
                    src: vec![a, b],
                    abs: 0,
                    neg: 0,
                    clamp: false,
                    omod: 0,
                    literal,
                    vop3: false,
                }),
                4 + 4 * usize::from(literal.is_some()),
            ));
        }
        _ => {}
    }
    match w >> 26 {
        0x34 => {
            let hi = next(4)?;
            let op = ((w >> 17) & 0x1FF) as u16;
            let count = valu_sources(op)?;
            let mut src = Vec::new();
            for index in 0..count {
                let field = ((hi >> (9 * index)) & 0x1FF) as u16;
                ensure!(field != 255, "GCN VOP3 instruction takes no literal");
                src.push(source(field, &mut literal, next(8))?);
            }
            let b = vop3b(op) || op < 256;
            return Ok((
                Op::Valu(Valu {
                    op,
                    dst: (w & 0xFF) as u8,
                    sdst: b.then_some(if op < 256 {
                        (w & 0xFF) as u8
                    } else {
                        ((w >> 8) & 0x7F) as u8
                    }),
                    src,
                    abs: if vop3b(op) { 0 } else { ((w >> 8) & 7) as u8 },
                    neg: ((hi >> 29) & 7) as u8,
                    clamp: !vop3b(op) && (w >> 11) & 1 != 0,
                    omod: ((hi >> 27) & 3) as u8,
                    literal: None,
                    vop3: true,
                }),
                8,
            ));
        }
        0x32 => {
            return Ok((
                Op::Vintrp {
                    op: ((w >> 16) & 3) as u8,
                    dst: ((w >> 18) & 0xFF) as u8,
                    attr: ((w >> 10) & 0x3F) as u8,
                    channel: ((w >> 8) & 3) as u8,
                },
                4,
            ));
        }
        0x36 => {
            return Ok((
                Op::Ds {
                    op: ((w >> 18) & 0xFF) as u8,
                },
                8,
            ));
        }
        0x38 | 0x3A => {
            let hi = next(4)?;
            let soffset = source(((hi >> 24) & 0xFF) as u16, &mut literal, next(8))?;
            ensure!(literal.is_none(), "GCN buffer offset takes no literal");
            let mtbuf = w >> 26 == 0x3A;
            let buffer = Buffer {
                op: if mtbuf {
                    ((w >> 16) & 7) as u8
                } else {
                    ((w >> 18) & 0x7F) as u8
                },
                offset: (w & 0xFFF) as u16,
                offen: (w >> 12) & 1 != 0,
                idxen: (w >> 13) & 1 != 0,
                dfmt: if mtbuf { ((w >> 19) & 0xF) as u8 } else { 0 },
                nfmt: if mtbuf { ((w >> 23) & 7) as u8 } else { 0 },
                vaddr: (hi & 0xFF) as u8,
                vdata: ((hi >> 8) & 0xFF) as u8,
                resource: (((hi >> 16) & 0x1F) << 2) as u8,
                soffset,
            };
            return Ok((
                if mtbuf {
                    Op::Mtbuf(buffer)
                } else {
                    Op::Mubuf(buffer)
                },
                8,
            ));
        }
        0x3C => {
            let hi = next(4)?;
            return Ok((
                Op::Mimg(Image {
                    op: ((w >> 18) & 0x7F) as u8,
                    dmask: ((w >> 8) & 0xF) as u8,
                    unorm: (w >> 12) & 1 != 0,
                    da: (w >> 14) & 1 != 0,
                    tfe: (w >> 16) & 1 != 0,
                    lwe: (w >> 17) & 1 != 0,
                    vaddr: (hi & 0xFF) as u8,
                    vdata: ((hi >> 8) & 0xFF) as u8,
                    resource: (((hi >> 16) & 0x1F) << 2) as u8,
                    sampler: (((hi >> 21) & 0x1F) << 2) as u8,
                }),
                8,
            ));
        }
        0x3E => {
            let hi = next(4)?;
            return Ok((
                Op::Exp(Export {
                    enable: (w & 0xF) as u8,
                    target: ((w >> 4) & 0x3F) as u8,
                    compressed: (w >> 10) & 1 != 0,
                    done: (w >> 11) & 1 != 0,
                    valid_mask: (w >> 12) & 1 != 0,
                    src: hi.to_le_bytes(),
                }),
                8,
            ));
        }
        _ => {}
    }
    if w >> 27 == 0x18 {
        let imm = (w >> 8) & 1 != 0;
        let offset = w & 0xFF;
        return Ok((
            Op::Smrd {
                op: ((w >> 22) & 0x1F) as u8,
                dst: ((w >> 15) & 0x7F) as u8,
                base: (((w >> 9) & 0x3F) << 1) as u8,
                offset: if imm {
                    SmrdOffset::Dwords(offset)
                } else {
                    SmrdOffset::Register
                },
            },
            4,
        ));
    }
    if w >> 28 == 0xB {
        return Ok((
            Op::Sopk {
                op: ((w >> 23) & 0x1F) as u8,
                dst: ((w >> 16) & 0x7F) as u8,
                imm: (w & 0xFFFF) as u16,
            },
            4,
        ));
    }
    if w >> 30 == 2 {
        let a = source((w & 0xFF) as u16, &mut literal, next(4))?;
        let b = source(((w >> 8) & 0xFF) as u16, &mut literal, next(4))?;
        return Ok((
            Op::Sop2 {
                op: ((w >> 23) & 0x7F) as u8,
                dst: ((w >> 16) & 0x7F) as u8,
                src: [a, b],
            },
            4 + 4 * usize::from(literal.is_some()),
        ));
    }
    ensure!(w >> 31 == 0, "Unknown GCN encoding {w:08X} at {at:#x}");
    // VOP2.
    let op = ((w >> 25) & 0x3F) as u16;
    let a = source((w & 0x1FF) as u16, &mut literal, next(4))?;
    let b = Src::Vgpr(((w >> 9) & 0xFF) as u8);
    let mut extra = None;
    // v_madmk_f32 and v_madak_f32 carry a literal after the instruction.
    if matches!(op, 32 | 33) && literal.is_none() {
        extra = Some(next(4)?);
    }
    let literal = literal.or(extra);
    Ok((
        Op::Valu(Valu {
            op: 256 + op,
            dst: ((w >> 17) & 0xFF) as u8,
            sdst: None,
            src: vec![a, b],
            abs: 0,
            neg: 0,
            clamp: false,
            omod: 0,
            literal,
            vop3: false,
        }),
        4 + 4 * usize::from(literal.is_some()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smrd_scales_base_and_reads_dword_offsets() {
        // s_load_dwordx4 s[16:19], s[2:3], 0x60 from program 80A7600D.
        let (op, size) = instruction(&0xC0880318u32.to_le_bytes(), 0).unwrap();
        assert_eq!(size, 4);
        let Op::Smrd { op, dst, base, offset } = op else { panic!() };
        assert_eq!((op, dst, base), (2, 16, 2));
        assert!(matches!(offset, SmrdOffset::Dwords(24)));
    }

    #[test]
    fn inline_constants_keep_their_bits() {
        let mut literal = None;
        assert_eq!(source(242, &mut literal, Ok(0)).unwrap(), Src::Const(0x3F80_0000));
        assert_eq!(source(193, &mut literal, Ok(0)).unwrap(), Src::Const(u32::MAX));
        assert_eq!(source(131, &mut literal, Ok(0)).unwrap(), Src::Const(3));
        assert!(literal.is_none());
    }
}
