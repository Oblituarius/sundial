//! Run a decoded GCN pixel program for one lane on the CPU.
//!
//! Instruction semantics are the lift's ([`super::lift`]), statement for statement: every
//! register a `u32`, lane masks this lane's bit, vector writes under EXEC, forward branches
//! skipping to their target. Inputs come from the caller: interpolants, constant buffer dwords
//! and texture samples, which are position independent (a sample returns what the caller
//! decides for that texture slot whatever its coordinates). A cross-quad swizzle reads the
//! lane's own value, as a lifted program does where its value is constant across the quad.
use super::{
    Header, Interface,
    decode::{self, Export, Image, Instruction, Op, SmrdOffset, Src, Valu},
    lift::{Desc, descriptors},
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::HashMap;

/// What the program reads from outside: interpolants, constant buffers and textures.
pub(in crate::d1_roi) trait Inputs {
    /// Lane `lane` of pixel interpolant `attribute`.
    fn interpolant(&self, attribute: u8, lane: u8) -> f32;
    /// The bits of dword `dword` of constant buffer `slot`.
    fn constant(&self, slot: u8, dword: u32) -> u32;
    /// A filtered or loaded texel of texture `slot`.
    fn sample(&self, slot: u8) -> [f32; 4];
}

/// A decoded pixel program ready to run.
pub(in crate::d1_roi) struct Program {
    header: Header,
    instructions: Vec<Instruction>,
}

/// The colour targets a run exported, `None` where a target was not written.
pub(in crate::d1_roi) type Targets = [Option<[f32; 4]>; 8];

impl Program {
    pub(in crate::d1_roi) fn new(header: Header, code: &[u8]) -> Result<Self> {
        ensure!(
            matches!(header.interface, Interface::Pixel { .. }),
            "Only pixel programs run on the CPU"
        );
        Ok(Self {
            instructions: decode::program(code)?,
            header,
        })
    }

    /// Run the program once. Returns `None` when the lane is discarded.
    pub(in crate::d1_roi) fn run(&self, inputs: &dyn Inputs) -> Result<Option<Targets>> {
        let (sgpr, extended) = descriptors(&self.header)?;
        let mut lane = Lane {
            inputs,
            descriptors: sgpr,
            extended,
            s: [0; 104],
            v: vec![0; 256],
            masks: [false; 128],
            exec: true,
            vcc: false,
            scc: false,
            targets: [None; 8],
            killed: false,
        };
        lane.prologue(&self.header)?;
        let mut skip_to = 0;
        for Instruction { at, op } in &self.instructions {
            if *at < skip_to {
                continue;
            }
            if let Some(target) = lane.step(*at, op)? {
                skip_to = target;
            }
        }
        Ok((!lane.killed).then_some(lane.targets))
    }
}

struct Lane<'a> {
    inputs: &'a dyn Inputs,
    descriptors: HashMap<u8, Desc>,
    extended: HashMap<u32, Desc>,
    s: [u32; 104],
    v: Vec<u32>,
    masks: [bool; 128],
    exec: bool,
    vcc: bool,
    scc: bool,
    targets: Targets,
    killed: bool,
}

fn mul_legacy(a: f32, b: f32) -> f32 {
    if a == 0.0 || b == 0.0 { 0.0 } else { a * b }
}

fn saturate(x: f32) -> f32 {
    if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }
}

fn f16_bits(x: f32) -> u32 {
    u32::from(half::f16::from_f32(x).to_bits())
}

fn f16_value(bits: u32) -> f32 {
    half::f16::from_bits((bits & 0xFFFF) as u16).to_f32()
}

/// GCN `v_cube*`: major axis z, then y, then x, ties to the later axis.
fn cube(op: u16, d: [f32; 3]) -> f32 {
    let a = d.map(f32::abs);
    let m = if a[2] >= a[0] && a[2] >= a[1] {
        2
    } else if a[1] >= a[0] {
        1
    } else {
        0
    };
    match op {
        324 => (m * 2 + usize::from(d[m] < 0.0)) as f32,
        325 => match m {
            2 => {
                if d[2] < 0.0 {
                    -d[0]
                } else {
                    d[0]
                }
            }
            1 => d[0],
            _ => {
                if d[0] < 0.0 {
                    d[2]
                } else {
                    -d[2]
                }
            }
        },
        326 => {
            if m == 1 {
                if d[1] < 0.0 { -d[2] } else { d[2] }
            } else {
                -d[1]
            }
        }
        _ => 2.0 * d[m],
    }
}

impl Lane<'_> {
    fn src(&self, src: Src) -> Result<u32> {
        Ok(match src {
            Src::Sgpr(r) => *self.s.get(usize::from(r)).context("GCN scalar operand")?,
            Src::Vgpr(r) => self.v[usize::from(r)],
            Src::Const(bits) => bits,
            Src::Scc => u32::from(self.scc),
            Src::VccLo => u32::from(self.vcc),
            Src::ExecLo => u32::from(self.exec),
            Src::M0 => 0,
            other => bail!("Unsupported GCN data operand {other:?}"),
        })
    }

    fn mask(&self, src: Src) -> Result<bool> {
        Ok(match src {
            Src::ExecLo => self.exec,
            Src::VccLo => self.vcc,
            Src::Sgpr(r) => self.masks[usize::from(r)],
            Src::Const(0) => false,
            Src::Const(u32::MAX) => true,
            other => bail!("Unsupported GCN lane mask {other:?}"),
        })
    }

    fn set_mask(&mut self, register: u8, value: bool) {
        match register {
            106 => self.vcc = value,
            126 => self.exec = value,
            r => self.masks[usize::from(r & 127)] = value,
        }
    }

    fn write_sgpr(&mut self, register: u8, value: u32) -> Result<()> {
        ensure!(
            register < 104,
            "Unsupported GCN scalar destination {register}"
        );
        self.descriptors.remove(&register);
        self.s[usize::from(register)] = value;
        Ok(())
    }

    fn write_vgpr(&mut self, register: u8, value: u32) {
        if self.exec {
            self.v[usize::from(register)] = value;
        }
    }

    fn float(&self, valu: &Valu, index: usize) -> Result<f32> {
        let mut value = f32::from_bits(self.src(valu.src[index])?);
        if valu.abs >> index & 1 != 0 {
            value = value.abs();
        }
        if valu.neg >> index & 1 != 0 {
            value = -value;
        }
        Ok(value)
    }

    fn float_result(valu: &Valu, value: f32) -> u32 {
        let value = match valu.omod {
            1 => value * 2.0,
            2 => value * 4.0,
            3 => value * 0.5,
            _ => value,
        };
        (if valu.clamp { saturate(value) } else { value }).to_bits()
    }

    fn valu(&mut self, valu: &Valu) -> Result<()> {
        let op = valu.op;
        if op < 256 {
            return self.compare(valu);
        }
        ensure!(
            !(353..=355).contains(&op),
            "GCN 64-bit shift is not evaluated"
        );
        let f = |i: usize| self.float(valu, i);
        let u = |i: usize| self.src(valu.src[i]);
        let dst = f32::from_bits(self.v[usize::from(valu.dst)]);
        let literal = || -> Result<f32> {
            Ok(f32::from_bits(
                valu.literal.context("GCN instruction literal is absent")?,
            ))
        };
        let float = |x: f32| Self::float_result(valu, x);
        let value = match op {
            256 => {
                let mask = match valu.src.get(2) {
                    Some(src) if valu.vop3 => self.mask(*src)?,
                    _ => self.vcc,
                };
                if mask { u(1)? } else { u(0)? }
            }
            259 => float(f(0)? + f(1)?),
            260 => float(f(0)? - f(1)?),
            261 => float(f(1)? - f(0)?),
            262 => float(mul_legacy(f(0)?, f(1)?) + dst),
            263 => float(mul_legacy(f(0)?, f(1)?)),
            264 => float(f(0)? * f(1)?),
            269 => {
                let (a, b) = (f(0)?, f(1)?);
                float(if a < b { a } else { b })
            }
            270 => {
                let (a, b) = (f(0)?, f(1)?);
                float(if a > b { a } else { b })
            }
            271 => float(f(0)?.min(f(1)?)),
            272 => float(f(0)?.max(f(1)?)),
            273 => (u(0)? as i32).min(u(1)? as i32) as u32,
            274 => (u(0)? as i32).max(u(1)? as i32) as u32,
            275 => u(0)?.min(u(1)?),
            276 => u(0)?.max(u(1)?),
            277 => u(0)? >> (u(1)? & 31),
            278 => u(1)? >> (u(0)? & 31),
            281 => u(0)? << (u(1)? & 31),
            282 => u(1)? << (u(0)? & 31),
            283 => u(0)? & u(1)?,
            284 => u(0)? | u(1)?,
            285 => u(0)? ^ u(1)?,
            287 => float(f(0)? * f(1)? + dst),
            288 => float(f(0)? * literal()? + f(1)?),
            289 => float(f(0)? * f(1)? + literal()?),
            293 => u(0)?.wrapping_add(u(1)?),
            294 => u(0)?.wrapping_sub(u(1)?),
            295 => u(1)?.wrapping_sub(u(0)?),
            303 => f16_bits(f(0)?) | f16_bits(f(1)?) << 16,
            320 => float(mul_legacy(f(0)?, f(1)?) + f(2)?),
            321 | 331 => float(f(0)? * f(1)? + f(2)?),
            324..=327 => float(cube(op, [f(0)?, f(1)?, f(2)?])),
            361 | 363 => u(0)?.wrapping_mul(u(1)?),
            385 => u(0)?,
            389 => float(u(0)? as i32 as f32),
            390 => float(u(0)? as f32),
            391 => f(0)? as u32,
            392 => (f(0)? as i32) as u32,
            394 => f16_bits(f(0)?),
            395 => float(f16_value(u(0)?)),
            416..=421 | 423 | 426 | 428 | 430 | 435 | 437 | 438 => {
                let a = f(0)?;
                float(match op {
                    416 => a - a.floor(),
                    417 => a.trunc(),
                    418 => a.ceil(),
                    419 => a.round_ties_even(),
                    420 => a.floor(),
                    421 => a.exp2(),
                    423 => a.log2(),
                    426 => 1.0 / a,
                    428 => (1.0 / a.sqrt()).min(f32::MAX),
                    430 => 1.0 / a.sqrt(),
                    435 => a.sqrt(),
                    437 => (a * 6.283_185_3).sin(),
                    _ => (a * 6.283_185_3).cos(),
                })
            }
            439 => !u(0)?,
            other => bail!("Unsupported GCN vector opcode {other}"),
        };
        if valu.sdst.is_some() && !matches!(op, 293..=295) {
            bail!("Unsupported GCN carry-out {op}");
        }
        self.write_vgpr(valu.dst, value);
        Ok(())
    }

    fn compare(&mut self, valu: &Valu) -> Result<()> {
        let op = valu.op;
        let (kind, sub, writes_exec) = match op {
            0..=15 => (0, op, false),
            16..=31 => (0, op - 16, true),
            128..=135 => (1, op - 128, false),
            144..=151 => (1, op - 144, true),
            192..=199 => (2, op - 192, false),
            208..=215 => (2, op - 208, true),
            other => bail!("Unsupported GCN compare {other}"),
        };
        let condition = if kind == 0 {
            let (a, b) = (self.float(valu, 0)?, self.float(valu, 1)?);
            match sub {
                0 => false,
                1 => a < b,
                2 => a == b,
                3 => a <= b,
                4 => a > b,
                5 => a < b || a > b,
                6 => a >= b,
                7 => !a.is_nan() && !b.is_nan(),
                8 => a.is_nan() || b.is_nan(),
                9 => !(a >= b),
                10 => !(a < b || a > b),
                11 => !(a > b),
                12 => !(a <= b),
                13 => a != b,
                14 => !(a < b),
                _ => true,
            }
        } else {
            let (a, b) = (self.src(valu.src[0])?, self.src(valu.src[1])?);
            let order = if kind == 1 {
                (a as i32).cmp(&(b as i32))
            } else {
                a.cmp(&b)
            };
            use std::cmp::Ordering::*;
            match sub {
                0 => false,
                1 => order == Less,
                2 => order == Equal,
                3 => order != Greater,
                4 => order == Greater,
                5 => order != Equal,
                6 => order != Less,
                _ => true,
            }
        };
        let value = self.exec && condition;
        let target = valu.sdst.unwrap_or(106);
        self.set_mask(target, value);
        if writes_exec {
            self.exec = value;
        }
        Ok(())
    }

    fn scalar_load(&mut self, op: u8, dst: u8, base: u8, offset: SmrdOffset) -> Result<()> {
        let count = match op & 7 {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 8,
            4 => 16,
            _ => bail!("Unsupported GCN scalar load {op}"),
        };
        let pointer = self.descriptors.get(&base).copied();
        let offset = match offset {
            SmrdOffset::Dwords(offset) => offset,
            SmrdOffset::Register(register) if op >= 8 && register < 104 => {
                let Some(Desc::Buffer(slot, 0)) = pointer else {
                    bail!("GCN buffer load through s{base}");
                };
                let first = self.s[usize::from(register)] >> 2;
                for k in 0..count {
                    let value = self.inputs.constant(slot, first + k);
                    self.write_sgpr(dst + k as u8, value)?;
                }
                return Ok(());
            }
            SmrdOffset::Register(register) => {
                bail!("GCN scalar load {op} has a register offset s{register}")
            }
        };
        if op < 8 {
            for k in 0..count {
                let desc = match pointer {
                    Some(Desc::Extended) => *self
                        .extended
                        .get(&(16 + offset + k))
                        .context("Extended user data is undeclared")?,
                    Some(Desc::Resources(api)) => Desc::Resource(
                        api + u8::try_from((offset + k) / 8)?,
                        ((offset + k) % 8) as u8,
                    ),
                    other => bail!("GCN scalar load through {other:?}"),
                };
                self.descriptors.insert(dst + k as u8, desc);
            }
            return Ok(());
        }
        let Some(Desc::Buffer(slot, 0)) = pointer else {
            bail!("GCN buffer load through {pointer:?}");
        };
        for k in 0..count {
            let value = self.inputs.constant(slot, offset + k);
            self.write_sgpr(dst + k as u8, value)?;
        }
        Ok(())
    }

    fn resource(&self, base: u8, kind: fn(u8, u8) -> Desc) -> Result<u8> {
        let desc = self.descriptors.get(&base).copied();
        let slot = match desc {
            Some(Desc::Resource(slot, 0) | Desc::Sampler(slot, 0) | Desc::Buffer(slot, 0)) => slot,
            other => bail!("GCN descriptor s{base} is {other:?}"),
        };
        ensure!(
            desc == Some(kind(slot, 0)),
            "GCN descriptor s{base} is another resource kind"
        );
        Ok(slot)
    }

    fn image(&mut self, image: &Image) -> Result<()> {
        ensure!(
            !image.da && !image.tfe && !image.lwe,
            "Unsupported GCN image addressing"
        );
        let texture = self.resource(image.resource, Desc::Resource)?;
        let texel = match image.op {
            // Sizes and mip counts: one texel, one level.
            14 => [1u32, 1, 1, 1].map(f32::from_bits),
            // Level of detail queries: the top level.
            96 => [0.0; 4],
            1 | 32 | 34 | 36 | 37 | 39 => self.inputs.sample(texture),
            other => bail!("Unsupported GCN image opcode {other}"),
        };
        let mut register = image.vdata;
        for (lane, value) in texel.into_iter().enumerate() {
            if image.dmask >> lane & 1 != 0 {
                self.write_vgpr(register, value.to_bits());
                register += 1;
            }
        }
        Ok(())
    }

    fn export(&mut self, export: &Export) -> Result<()> {
        if export.valid_mask && export.done && !self.exec {
            self.killed = true;
        }
        let target = match export.target {
            0..=7 => usize::from(export.target),
            9 => return Ok(()),
            other => bail!("Unsupported GCN export target {other}"),
        };
        let mut lanes = self.targets[target].unwrap_or([0.0; 4]);
        for (lane, value) in lanes.iter_mut().enumerate() {
            if export.enable >> lane & 1 == 0 {
                continue;
            }
            *value = if export.compressed {
                let register = self.v[usize::from(export.src[lane / 2])];
                f16_value(register >> (16 * (lane % 2)))
            } else {
                f32::from_bits(self.v[usize::from(export.src[lane])])
            };
        }
        self.targets[target] = Some(lanes);
        Ok(())
    }

    fn prologue(&mut self, header: &Header) -> Result<()> {
        let Interface::Pixel { input_addr, .. } = header.interface else {
            unreachable!()
        };
        const SIZES: [u8; 16] = [2, 2, 2, 3, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1];
        let mut vgpr = 0u8;
        for (bit, size) in SIZES.iter().enumerate() {
            if input_addr >> bit & 1 == 0 {
                continue;
            }
            match bit {
                0..=7 => {}
                // The pixel centre of the first pixel, at depth one half.
                8..=11 => self.v[usize::from(vgpr)] = [0.5f32, 0.5, 0.5, 1.0][bit - 8].to_bits(),
                12 => self.v[usize::from(vgpr)] = 1f32.to_bits(),
                _ => bail!("Unsupported pixel system input {bit}"),
            }
            vgpr += size;
        }
        Ok(())
    }

    /// Run one instruction. Returns the byte offset a taken forward branch skips to.
    fn step(&mut self, at: usize, op: &Op) -> Result<Option<usize>> {
        match op {
            Op::Sopp { op, imm } => {
                let taken = match op {
                    0 | 1 | 10 | 12 | 15 => return Ok(None),
                    2 => true,
                    4 => !self.scc,
                    5 => self.scc,
                    6 => !self.vcc,
                    7 => self.vcc,
                    8 => !self.exec,
                    9 => self.exec,
                    other => bail!("Unsupported GCN program-control opcode {other}"),
                };
                let target = usize::try_from(at as i64 + 4 + i64::from(*imm as i16) * 4)?;
                ensure!(target > at, "GCN program has a backward branch (loop)");
                return Ok(taken.then_some(target));
            }
            Op::Sop1 { op, dst, src } => match op {
                3 => {
                    if let Src::Sgpr(r) = src
                        && let Some(desc) = self.descriptors.get(r).copied()
                    {
                        self.descriptors.insert(*dst, desc);
                    } else if !matches!(dst, 107 | 124) {
                        let value = self.src(*src)?;
                        self.write_sgpr(*dst, value)?;
                    }
                }
                4 => {
                    if let Src::Sgpr(r) = src
                        && let Some(desc) = self.descriptors.get(r).copied()
                    {
                        self.descriptors.insert(*dst, desc);
                        if let Some(high) = self.descriptors.get(&(r + 1)).copied() {
                            self.descriptors.insert(dst + 1, high);
                        }
                    } else {
                        let value = self.mask(*src)?;
                        self.set_mask(*dst, value);
                    }
                }
                10 => {
                    ensure!(*dst == 126, "Unsupported GCN whole-quad mode target");
                    self.exec = true;
                }
                33 => ensure!(
                    matches!(src, Src::Sgpr(r) if self.descriptors.get(r) == Some(&Desc::FetchShader)),
                    "GCN subroutine call is not the fetch shader"
                ),
                36 | 37 | 39 => {
                    let source = self.mask(*src)?;
                    let exec = self.exec;
                    self.set_mask(*dst, exec);
                    self.exec = match op {
                        36 => source && exec,
                        37 => source || exec,
                        _ => source && !exec,
                    };
                    self.scc = self.exec;
                }
                other => bail!("Unsupported GCN scalar opcode SOP1 {other}"),
            },
            Op::Sop2 { op, dst, src } => {
                if matches!(op, 15 | 17 | 19 | 21 | 23) {
                    let (a, b) = (self.mask(src[0])?, self.mask(src[1])?);
                    let value = match op {
                        15 => a && b,
                        17 => a || b,
                        19 => a != b,
                        21 => a && !b,
                        _ => a || !b,
                    };
                    self.set_mask(*dst, value);
                    self.scc = value;
                } else {
                    let (a, b) = (self.src(src[0])?, self.src(src[1])?);
                    let value = match op {
                        0 | 2 => a.wrapping_add(b),
                        1 | 3 => a.wrapping_sub(b),
                        14 => a & b,
                        16 => a | b,
                        18 => a ^ b,
                        30 => a << (b & 31),
                        32 => a >> (b & 31),
                        38 => a.wrapping_mul(b),
                        other => bail!("Unsupported GCN scalar opcode SOP2 {other}"),
                    };
                    self.write_sgpr(*dst, value)?;
                }
            }
            Op::Sopc { op, src } => {
                ensure!(*op < 12, "Unsupported GCN scalar compare {op}");
                let (a, b) = (self.src(src[0])?, self.src(src[1])?);
                let order = if *op < 6 {
                    (a as i32).cmp(&(b as i32))
                } else {
                    a.cmp(&b)
                };
                use std::cmp::Ordering::*;
                self.scc = match op % 6 {
                    0 => order == Equal,
                    1 => order != Equal,
                    2 => order == Greater,
                    3 => order != Less,
                    4 => order == Less,
                    _ => order != Greater,
                };
            }
            Op::Sopk { op, dst, imm } => {
                let imm = *imm as i16 as i32 as u32;
                let value = match op {
                    0 => imm,
                    15 => self.s[usize::from(*dst)].wrapping_add(imm),
                    other => bail!("Unsupported GCN scalar opcode SOPK {other}"),
                };
                self.write_sgpr(*dst, value)?;
            }
            Op::Smrd {
                op,
                dst,
                base,
                offset,
            } => self.scalar_load(*op, *dst, *base, *offset)?,
            Op::Valu(valu) => {
                ensure!(
                    !matches!(valu.op, 257 | 258 | 386),
                    "GCN program reads across lanes"
                );
                self.valu(valu)?;
            }
            Op::Vintrp {
                op,
                dst,
                attr,
                channel,
            } => {
                ensure!(*op < 2, "Unsupported GCN interpolation {op}");
                let value = self.inputs.interpolant(*attr, *channel).to_bits();
                self.write_vgpr(*dst, value);
            }
            Op::Ds {
                op: 53,
                offset,
                gds: false,
                addr,
                vdst,
            } if offset & 0x8000 != 0 => {
                let value = self.v[usize::from(*addr)];
                self.write_vgpr(*vdst, value);
            }
            Op::Ds { op, .. } => bail!("GCN program uses local data share opcode {op}"),
            Op::Mtbuf(buffer) => {
                ensure!(
                    buffer.op <= 3
                        && buffer.idxen
                        && !buffer.offen
                        && buffer.offset == 0
                        && buffer.soffset == Src::Const(0)
                        && buffer.dfmt == 14
                        && buffer.nfmt == 7,
                    "Unsupported GCN typed buffer load"
                );
                let slot = self.resource(buffer.resource, Desc::Buffer)?;
                let row = self.v[usize::from(buffer.vaddr)];
                for lane in 0..=buffer.op {
                    let value = self.inputs.constant(slot, row * 4 + u32::from(lane));
                    self.write_vgpr(buffer.vdata + lane, value);
                }
            }
            Op::Mubuf(buffer) => bail!("Unsupported GCN buffer opcode {}", buffer.op),
            Op::Mimg(image) => self.image(image)?,
            Op::Exp(export) => self.export(export)?,
        }
        Ok(None)
    }
}
