//! Lift a decoded GCN program to HLSL, one invocation per lane.
//!
//! Every register is a `uint`. Lane masks (EXEC, VCC and SGPR pairs written by compares
//! and saves) become this lane's bit, a `bool`. Forward branches become guards on the
//! instructions they skip, which is exact for the uniform and EXEC-zero skips the
//! compiler emits. Resource descriptors are tracked symbolically from the Gnm input
//! usage slots, so each sample, constant load and buffer fetch names the API register
//! (`t#`, `s#`, `cb#`) the D1 Xbox build declares for the same program.
use super::{
    Header, Interface,
    decode::{self, Buffer, Export, Image, Instruction, Op, SmrdOffset, Src, Valu},
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Desc {
    /// Extended user data: user registers 16 and up, read through this pointer.
    Extended,
    /// A table of 8-dword image descriptors starting at an API texture slot.
    Resources(u8),
    VertexTable,
    FetchShader,
    Resource(u8, u8),
    Sampler(u8, u8),
    Buffer(u8, u8),
}

/// Native vertex element semantic codes, as Shadowkeep's layouts store them.
fn semantic_name(code: u8) -> Result<(&'static str, bool)> {
    Ok(match code {
        0 => ("POSITION", false),
        1 => ("BLENDWEIGHT", false),
        2 => ("BLENDINDICES", true),
        3 => ("NORMAL", false),
        5 => ("TEXCOORD", false),
        6 => ("TANGENT", false),
        8 => ("COLOR", false),
        other => bail!("Unsupported vertex element semantic {other}"),
    })
}

const LANES: [char; 4] = ['x', 'y', 'z', 'w'];

struct Lift<'a> {
    header: &'a Header,
    body: String,
    sgpr: HashMap<u8, Desc>,
    extended: HashMap<u32, Desc>,
    sgprs: BTreeSet<u8>,
    vgprs: BTreeSet<u8>,
    masks: BTreeSet<u8>,
    /// Constant buffer slot → rows read, or `None` when indexed by a register.
    cbuffers: BTreeMap<u8, Option<u32>>,
    textures: BTreeSet<u8>,
    /// Texture slots the material binds cube maps to. GCN samples a cube through its T#
    /// type, with face coordinates the program computes (`v_cube*`), and no `da` bit.
    cubes: &'a BTreeSet<u8>,
    samplers: BTreeSet<u8>,
    /// Active forward-branch skips: guard variable and the byte offset it ends at.
    skips: Vec<(String, usize)>,
    branches: usize,
    temps: usize,
    /// Pixel colour targets exported.
    targets: BTreeSet<u8>,
    /// The vertex program exports a position.
    position: bool,
}

impl<'a> Lift<'a> {
    fn new(header: &'a Header, cubes: &'a BTreeSet<u8>) -> Result<Self> {
        let mut lift = Self {
            header,
            body: String::new(),
            sgpr: HashMap::new(),
            extended: HashMap::new(),
            sgprs: BTreeSet::new(),
            vgprs: BTreeSet::new(),
            masks: BTreeSet::new(),
            cbuffers: BTreeMap::new(),
            textures: BTreeSet::new(),
            cubes,
            samplers: BTreeSet::new(),
            skips: Vec::new(),
            branches: 0,
            temps: 0,
            targets: BTreeSet::new(),
            position: false,
        };
        for slot in &header.slots {
            let (count, desc): (u8, Box<dyn Fn(u8) -> Desc>) = match slot.kind {
                0 => (
                    if slot.flags & 1 != 0 { 8 } else { 4 },
                    Box::new(|k| Desc::Resource(slot.api, k)),
                ),
                1 => (4, Box::new(|k| Desc::Sampler(slot.api, k))),
                2 => (4, Box::new(|k| Desc::Buffer(slot.api, k))),
                0x12 => (2, Box::new(|_| Desc::FetchShader)),
                0x13 => (2, Box::new(|_| Desc::Resources(slot.api))),
                0x17 => (2, Box::new(|_| Desc::VertexTable)),
                0x1B => (2, Box::new(|_| Desc::Extended)),
                other => bail!("Unsupported Gnm input usage {other:#x}"),
            };
            for k in 0..count {
                let register = u32::from(slot.register) + u32::from(k);
                if register < 16 {
                    lift.sgpr.insert(register as u8, desc(k));
                } else {
                    lift.extended.insert(register, desc(k));
                }
            }
        }
        Ok(lift)
    }

    fn line(&mut self, text: impl AsRef<str>) {
        self.body.push_str("    ");
        self.body.push_str(text.as_ref());
        self.body.push('\n');
    }

    /// The guard of the current instruction: no active skip, and for vector writes EXEC.
    fn guard(&self, vector: bool) -> Option<String> {
        let mut terms = self
            .skips
            .iter()
            .map(|(flag, _)| format!("!{flag}"))
            .collect::<Vec<_>>();
        if vector {
            terms.push("exec".to_owned());
        }
        (!terms.is_empty()).then(|| terms.join(" && "))
    }

    /// Compute `value` unconditionally, then assign it to `target` under the guard.
    fn assign(&mut self, target: String, value: String, vector: bool) {
        match self.guard(vector) {
            Some(guard) => {
                let temp = format!("r{}", self.temps);
                self.temps += 1;
                let ty = if target.starts_with("km") || target == "vcc" || target == "exec" || target == "scc" {
                    "bool"
                } else {
                    "uint"
                };
                self.line(format!("{ty} {temp} = {value};"));
                self.line(format!("if ({guard}) {target} = {temp};"));
            }
            None => self.line(format!("{target} = {value};")),
        }
    }

    fn s(&mut self, register: u8) -> String {
        self.sgprs.insert(register);
        format!("sg{register}")
    }

    fn v(&mut self, register: u8) -> String {
        self.vgprs.insert(register);
        format!("vg{register}")
    }

    /// A 32-bit source as a `uint` expression.
    fn src(&mut self, src: Src) -> Result<String> {
        Ok(match src {
            Src::Sgpr(r) => self.s(r),
            Src::Vgpr(r) => self.v(r),
            Src::Const(bits) => format!("{bits}u"),
            Src::Scc => "(scc ? 1u : 0u)".to_owned(),
            Src::VccLo => "(vcc ? 1u : 0u)".to_owned(),
            Src::ExecLo => "(exec ? 1u : 0u)".to_owned(),
            Src::M0 => "0u".to_owned(),
            other => bail!("Unsupported GCN data operand {other:?}"),
        })
    }

    /// A 64-bit lane-mask source as this lane's bit.
    fn mask(&mut self, src: Src) -> Result<String> {
        Ok(match src {
            Src::ExecLo => "exec".to_owned(),
            Src::VccLo => "vcc".to_owned(),
            Src::Sgpr(r) => {
                self.masks.insert(r);
                format!("km{r}")
            }
            Src::Const(0) => "false".to_owned(),
            Src::Const(u32::MAX) => "true".to_owned(),
            other => bail!("Unsupported GCN lane mask {other:?}"),
        })
    }

    fn mask_target(&mut self, register: u8) -> String {
        match register {
            106 => "vcc".to_owned(),
            126 => "exec".to_owned(),
            r if r < 104 => {
                self.masks.insert(r);
                format!("km{r}")
            }
            r => format!("km{r}"),
        }
    }

    /// Write a scalar register, which drops any descriptor it held.
    fn write_sgpr(&mut self, register: u8, value: String) -> Result<()> {
        ensure!(register < 104, "Unsupported GCN scalar destination {register}");
        self.sgpr.remove(&register);
        let target = self.s(register);
        self.assign(target, value, false);
        Ok(())
    }

    fn float(&mut self, valu: &Valu, index: usize) -> Result<String> {
        let mut value = format!("asfloat({})", self.src(valu.src[index])?);
        if valu.abs >> index & 1 != 0 {
            value = format!("abs({value})");
        }
        if valu.neg >> index & 1 != 0 {
            value = format!("(-{value})");
        }
        Ok(value)
    }

    fn float_result(&self, valu: &Valu, value: String) -> String {
        let mut value = match valu.omod {
            1 => format!("({value} * 2.0f)"),
            2 => format!("({value} * 4.0f)"),
            3 => format!("({value} * 0.5f)"),
            _ => value,
        };
        if valu.clamp {
            value = format!("saturate({value})");
        }
        format!("asuint({value})")
    }

    fn valu(&mut self, valu: &Valu) -> Result<()> {
        let op = valu.op;
        if op < 256 {
            return self.compare(valu);
        }
        let dst = self.v(valu.dst);
        let f = |lift: &mut Self, i: usize| lift.float(valu, i);
        let u = |lift: &mut Self, i: usize| lift.src(valu.src[i]);
        let literal = || valu.literal.context("GCN instruction literal is absent");
        let value = match op {
            256 => {
                let mask = match valu.src.get(2) {
                    Some(src) if valu.vop3 => self.mask(*src)?,
                    _ => "vcc".to_owned(),
                };
                format!("({mask} ? {} : {})", u(self, 1)?, u(self, 0)?)
            }
            259 => {
                let v = format!("({} + {})", f(self, 0)?, f(self, 1)?);
                self.float_result(valu, v)
            }
            260 => {
                let v = format!("({} - {})", f(self, 0)?, f(self, 1)?);
                self.float_result(valu, v)
            }
            261 => {
                let v = format!("({} - {})", f(self, 1)?, f(self, 0)?);
                self.float_result(valu, v)
            }
            262 => {
                let v = format!("(mul_legacy({}, {}) + asfloat({dst}))", f(self, 0)?, f(self, 1)?);
                self.float_result(valu, v)
            }
            263 => {
                let v = format!("mul_legacy({}, {})", f(self, 0)?, f(self, 1)?);
                self.float_result(valu, v)
            }
            264 => {
                let v = format!("({} * {})", f(self, 0)?, f(self, 1)?);
                self.float_result(valu, v)
            }
            269 | 271 => {
                let (a, b) = (f(self, 0)?, f(self, 1)?);
                let v = if op == 269 {
                    format!("(({a}) < ({b}) ? ({a}) : ({b}))")
                } else {
                    format!("min({a}, {b})")
                };
                self.float_result(valu, v)
            }
            270 | 272 => {
                let (a, b) = (f(self, 0)?, f(self, 1)?);
                let v = if op == 270 {
                    format!("(({a}) > ({b}) ? ({a}) : ({b}))")
                } else {
                    format!("max({a}, {b})")
                };
                self.float_result(valu, v)
            }
            273 => format!("asuint(min(asint({}), asint({})))", u(self, 0)?, u(self, 1)?),
            274 => format!("asuint(max(asint({}), asint({})))", u(self, 0)?, u(self, 1)?),
            275 => format!("min({}, {})", u(self, 0)?, u(self, 1)?),
            276 => format!("max({}, {})", u(self, 0)?, u(self, 1)?),
            277 => format!("({} >> ({} & 31u))", u(self, 0)?, u(self, 1)?),
            278 => format!("({} >> ({} & 31u))", u(self, 1)?, u(self, 0)?),
            281 => format!("({} << ({} & 31u))", u(self, 0)?, u(self, 1)?),
            282 => format!("({} << ({} & 31u))", u(self, 1)?, u(self, 0)?),
            283 => format!("({} & {})", u(self, 0)?, u(self, 1)?),
            284 => format!("({} | {})", u(self, 0)?, u(self, 1)?),
            285 => format!("({} ^ {})", u(self, 0)?, u(self, 1)?),
            287 => {
                let v = format!("({} * {} + asfloat({dst}))", f(self, 0)?, f(self, 1)?);
                self.float_result(valu, v)
            }
            288 => {
                let k = literal()?;
                let v = format!("({} * asfloat({k}u) + {})", f(self, 0)?, f(self, 1)?);
                self.float_result(valu, v)
            }
            289 => {
                let k = literal()?;
                let v = format!("({} * {} + asfloat({k}u))", f(self, 0)?, f(self, 1)?);
                self.float_result(valu, v)
            }
            293 => format!("({} + {})", u(self, 0)?, u(self, 1)?),
            294 => format!("({} - {})", u(self, 0)?, u(self, 1)?),
            295 => format!("({} - {})", u(self, 1)?, u(self, 0)?),
            303 => format!("(f32tof16({}) | (f32tof16({}) << 16))", f(self, 0)?, f(self, 1)?),
            320 => {
                let v = format!("(mul_legacy({}, {}) + {})", f(self, 0)?, f(self, 1)?, f(self, 2)?);
                self.float_result(valu, v)
            }
            321 | 331 => {
                let v = format!("({} * {} + {})", f(self, 0)?, f(self, 1)?, f(self, 2)?);
                self.float_result(valu, v)
            }
            // v_cubeid, v_cubesc, v_cubetc and v_cubema over the vector (src0, src1, src2).
            324..=327 => {
                let (x, y, z) = (f(self, 0)?, f(self, 1)?, f(self, 2)?);
                let name = ["cube_id", "cube_sc", "cube_tc", "cube_ma"][usize::from(op - 324)];
                self.float_result(valu, format!("{name}(float3({x}, {y}, {z}))"))
            }
            361 | 363 => format!("({} * {})", u(self, 0)?, u(self, 1)?),
            385 => u(self, 0)?,
            389 => {
                let v = format!("(float)asint({})", u(self, 0)?);
                self.float_result(valu, v)
            }
            390 => {
                let v = format!("(float){}", u(self, 0)?);
                self.float_result(valu, v)
            }
            391 => format!("(uint){}", f(self, 0)?),
            392 => format!("asuint((int){})", f(self, 0)?),
            394 => format!("f32tof16({})", f(self, 0)?),
            395 => {
                let v = format!("f16tof32({})", u(self, 0)?);
                self.float_result(valu, v)
            }
            416..=421 | 423 | 426 | 428 | 430 | 435 | 437 | 438 => {
                let a = f(self, 0)?;
                let v = match op {
                    416 => format!("frac({a})"),
                    417 => format!("trunc({a})"),
                    418 => format!("ceil({a})"),
                    419 => format!("round({a})"),
                    420 => format!("floor({a})"),
                    421 => format!("exp2({a})"),
                    423 => format!("log2({a})"),
                    426 => format!("rcp({a})"),
                    428 => format!("rsq_clamp({a})"),
                    430 => format!("rsqrt({a})"),
                    435 => format!("sqrt({a})"),
                    // GCN sine and cosine take revolutions.
                    437 => format!("sin({a} * 6.2831853f)"),
                    438 => format!("cos({a} * 6.2831853f)"),
                    _ => unreachable!(),
                };
                self.float_result(valu, v)
            }
            439 => format!("(~{})", u(self, 0)?),
            other => bail!("Unsupported GCN vector opcode {other}"),
        };
        if valu.sdst.is_some() && !matches!(op, 293..=295) {
            bail!("Unsupported GCN carry-out {op}");
        }
        self.assign(dst, value, true);
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
                0 => "false".to_owned(),
                1 => format!("({a} < {b})"),
                2 => format!("({a} == {b})"),
                3 => format!("({a} <= {b})"),
                4 => format!("({a} > {b})"),
                5 => format!("({a} < {b} || {a} > {b})"),
                6 => format!("({a} >= {b})"),
                7 => format!("({a} == {a} && {b} == {b})"),
                8 => format!("({a} != {a} || {b} != {b})"),
                9 => format!("!({a} >= {b})"),
                10 => format!("!({a} < {b} || {a} > {b})"),
                11 => format!("!({a} > {b})"),
                12 => format!("!({a} <= {b})"),
                13 => format!("!({a} == {b})"),
                14 => format!("!({a} < {b})"),
                _ => "true".to_owned(),
            }
        } else {
            let (mut a, mut b) = (self.src(valu.src[0])?, self.src(valu.src[1])?);
            if kind == 1 {
                a = format!("asint({a})");
                b = format!("asint({b})");
            }
            match sub {
                0 => "false".to_owned(),
                1 => format!("({a} < {b})"),
                2 => format!("({a} == {b})"),
                3 => format!("({a} <= {b})"),
                4 => format!("({a} > {b})"),
                5 => format!("({a} != {b})"),
                6 => format!("({a} >= {b})"),
                _ => "true".to_owned(),
            }
        };
        let target = match valu.sdst {
            Some(register) => self.mask_target(register),
            None => "vcc".to_owned(),
        };
        // Inactive lanes write zero bits.
        let value = format!("(exec && {condition})");
        self.assign(target.clone(), value, false);
        if writes_exec {
            self.assign("exec".to_owned(), target, false);
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
        let SmrdOffset::Dwords(offset) = offset else {
            bail!("GCN scalar load has a register offset")
        };
        let pointer = self.sgpr.get(&base).copied();
        if op < 8 {
            for k in 0..count {
                let desc = match pointer {
                    Some(Desc::Extended) => *self
                        .extended
                        .get(&(16 + offset + k))
                        .with_context(|| format!("Extended user data {} is undeclared", 16 + offset + k))?,
                    Some(Desc::Resources(api)) => Desc::Resource(
                        api + u8::try_from((offset + k) / 8)?,
                        ((offset + k) % 8) as u8,
                    ),
                    other => bail!("GCN scalar load through {other:?}"),
                };
                let register = dst + k as u8;
                self.sgpr.insert(register, desc);
            }
            return Ok(());
        }
        let Some(Desc::Buffer(slot, 0)) = pointer else {
            bail!("GCN buffer load through {pointer:?}");
        };
        for k in 0..count {
            let dword = offset + k;
            let row = dword / 4;
            let rows = self.cbuffers.entry(slot).or_insert(Some(0));
            if let Some(rows) = rows {
                *rows = (*rows).max(row + 1);
            }
            let value = format!("asuint(cb{slot}[{row}].{})", LANES[(dword % 4) as usize]);
            self.write_sgpr(dst + k as u8, value)?;
        }
        Ok(())
    }

    /// The API slot an image instruction's descriptor registers name.
    fn resource(&self, base: u8, count: u8, kind: fn(u8, u8) -> Desc) -> Result<u8> {
        let first = self.sgpr.get(&base).copied();
        let slot = match first {
            Some(Desc::Resource(slot, 0)) | Some(Desc::Sampler(slot, 0)) | Some(Desc::Buffer(slot, 0)) => slot,
            other => bail!("GCN descriptor s{base} is {other:?}"),
        };
        for k in 0..count {
            ensure!(
                self.sgpr.get(&(base + k)) == Some(&kind(slot, k)),
                "GCN descriptor s[{base}:{}] is not one API resource",
                base + count - 1
            );
        }
        Ok(slot)
    }

    fn image(&mut self, image: &Image) -> Result<()> {
        ensure!(!image.da, "Unsupported GCN array image addressing (op {})", image.op);
        // Texel loads address in integers whatever the unnormalized bit says.
        ensure!(
            !image.unorm || image.op <= 1,
            "Unsupported GCN unnormalized image addressing (op {})",
            image.op
        );
        ensure!(!image.tfe && !image.lwe, "Unsupported GCN image fault reporting");
        let texture = self.resource(image.resource, 8, Desc::Resource)?;
        self.textures.insert(texture);
        let v = |lift: &mut Self, k: u8| -> String {
            let register = lift.v(image.vaddr + k);
            format!("asfloat({register})")
        };
        if self.cubes.contains(&texture) {
            return self.cube_image(image, texture);
        }
        let call = match image.op {
            1 => {
                let x = self.v(image.vaddr);
                let y = self.v(image.vaddr + 1);
                let mip = self.v(image.vaddr + 2);
                format!("tex{texture}.Load(int3(asint({x}), asint({y}), asint({mip})))")
            }
            32 | 36 | 37 | 39 | 34 | 96 => {
                let sampler = self.resource(image.sampler, 4, Desc::Sampler)?;
                self.samplers.insert(sampler);
                match image.op {
                    96 => {
                        let uv = format!("float2({}, {})", v(self, 0), v(self, 1));
                        format!(
                            "float4(tex{texture}.CalculateLevelOfDetail(smp{sampler}, {uv}), tex{texture}.CalculateLevelOfDetailUnclamped(smp{sampler}, {uv}), 0, 0)"
                        )
                    }
                    32 => format!("tex{texture}.Sample(smp{sampler}, float2({}, {}))", v(self, 0), v(self, 1)),
                    36 => format!(
                        "tex{texture}.SampleLevel(smp{sampler}, float2({}, {}), {})",
                        v(self, 0),
                        v(self, 1),
                        v(self, 2)
                    ),
                    37 => format!(
                        "tex{texture}.SampleBias(smp{sampler}, float2({}, {}), {})",
                        v(self, 1),
                        v(self, 2),
                        v(self, 0)
                    ),
                    39 => format!("tex{texture}.SampleLevel(smp{sampler}, float2({}, {}), 0)", v(self, 0), v(self, 1)),
                    _ => format!(
                        "tex{texture}.SampleGrad(smp{sampler}, float2({}, {}), float2({}, {}), float2({}, {}))",
                        v(self, 4),
                        v(self, 5),
                        v(self, 0),
                        v(self, 1),
                        v(self, 2),
                        v(self, 3)
                    ),
                }
            }
            other => bail!("Unsupported GCN image opcode {other}"),
        };
        self.image_result(image, &call);
        Ok(())
    }

    /// Sample a cube map. The program addresses it as GCN does, face coordinates offset by
    /// 1.5 and a face index (`cube_dir` turns them back into a direction); the coordinate
    /// order of each opcode is that of its 2D form, with the face after t.
    fn cube_image(&mut self, image: &Image, texture: u8) -> Result<()> {
        let sampler = self.resource(image.sampler, 4, Desc::Sampler)?;
        self.samplers.insert(sampler);
        let mut v = |k: u8| -> String {
            let register = self.v(image.vaddr + k);
            format!("asfloat({register})")
        };
        let call = match image.op {
            32 => format!("tex{texture}.Sample(smp{sampler}, cube_dir({}, {}, {}))", v(0), v(1), v(2)),
            36 => format!(
                "tex{texture}.SampleLevel(smp{sampler}, cube_dir({}, {}, {}), {})",
                v(0),
                v(1),
                v(2),
                v(3)
            ),
            37 => format!(
                "tex{texture}.SampleBias(smp{sampler}, cube_dir({}, {}, {}), {})",
                v(1),
                v(2),
                v(3),
                v(0)
            ),
            39 => format!("tex{texture}.SampleLevel(smp{sampler}, cube_dir({}, {}, {}), 0)", v(0), v(1), v(2)),
            96 => {
                let direction = format!("cube_dir({}, {}, {})", v(0), v(1), v(2));
                format!(
                    "float4(tex{texture}.CalculateLevelOfDetail(smp{sampler}, {direction}), tex{texture}.CalculateLevelOfDetailUnclamped(smp{sampler}, {direction}), 0, 0)"
                )
            }
            other => bail!("Unsupported GCN cube image opcode {other}"),
        };
        self.image_result(image, &call);
        Ok(())
    }

    /// Store an image result's enabled lanes in consecutive registers.
    fn image_result(&mut self, image: &Image, call: &str) {
        let temp = format!("r{}", self.temps);
        self.temps += 1;
        self.line(format!("float4 {temp} = {call};"));
        let mut register = image.vdata;
        for (lane, name) in LANES.iter().enumerate() {
            if image.dmask >> lane & 1 != 0 {
                let target = self.v(register);
                self.assign(target, format!("asuint({temp}.{name})"), true);
                register += 1;
            }
        }
    }

    fn typed_load(&mut self, buffer: &Buffer) -> Result<()> {
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
        let slot = self.resource(buffer.resource, 4, Desc::Buffer)?;
        self.cbuffers.insert(slot, None);
        let index = self.v(buffer.vaddr);
        let temp = format!("r{}", self.temps);
        self.temps += 1;
        self.line(format!("float4 {temp} = cb{slot}[{index}];"));
        for (lane, name) in LANES.iter().enumerate().take(usize::from(buffer.op) + 1) {
            let target = self.v(buffer.vdata + lane as u8);
            self.assign(target, format!("asuint({temp}.{name})"), true);
        }
        Ok(())
    }

    /// Write an export's lanes to its output when it executes; later instructions may
    /// reuse the registers it read.
    fn export(&mut self, export: &Export) -> Result<()> {
        let vertex = matches!(self.header.interface, Interface::Vertex { .. });
        let name = match (vertex, export.target) {
            (false, 0..=7) => {
                self.targets.insert(export.target);
                format!("o{}", export.target)
            }
            (false, 9) => String::new(),
            (true, 12) => {
                self.position = true;
                "position".to_owned()
            }
            (true, 32..=63) => {
                let param = export.target - 32;
                let Interface::Vertex { exports, .. } = &self.header.interface else {
                    unreachable!()
                };
                ensure!(
                    exports.iter().any(|(_, p)| *p == param),
                    "GCN vertex export {param} has no semantic"
                );
                format!("o{param}")
            }
            (_, other) => bail!("Unsupported GCN export target {other}"),
        };
        let mut lanes: [Option<String>; 4] = Default::default();
        if export.compressed {
            for half in 0..2 {
                let register = self.v(export.src[half]);
                for k in 0..2 {
                    if export.enable >> (half * 2 + k) & 1 != 0 {
                        lanes[half * 2 + k] =
                            Some(format!("f16tof32({register} >> {})", 16 * k));
                    }
                }
            }
        } else {
            for (lane, value) in lanes.iter_mut().enumerate() {
                if export.enable >> lane & 1 != 0 {
                    let register = self.v(export.src[lane]);
                    *value = Some(format!("asfloat({register})"));
                }
            }
        }
        let guard = self.guard(false);
        let mut lines = Vec::new();
        if !vertex && export.valid_mask && export.done {
            // Lanes whose EXEC bit is clear at the final export are killed.
            lines.push("if (!exec) discard;".to_owned());
        }
        if !name.is_empty() {
            for (lane, value) in lanes.into_iter().enumerate() {
                if let Some(value) = value {
                    lines.push(format!("{name}.{} = {value};", LANES[lane]));
                }
            }
        }
        for line in lines {
            match &guard {
                Some(guard) => self.line(format!("if ({guard}) {{ {line} }}")),
                None => self.line(line),
            }
        }
        Ok(())
    }
}

impl Lift<'_> {
    /// Write the VGPRs the hardware fills before the first instruction.
    fn prologue(&mut self, elements: &[(u8, u8)]) -> Result<()> {
        let mut values = Vec::new();
        match &self.header.interface {
            Interface::Vertex { inputs, .. } => {
                for &(element, vgpr, size) in inputs {
                    let (code, _) = *elements
                        .get(usize::from(element))
                        .with_context(|| format!("Vertex input {element} is not a layout element"))?;
                    let (_, integer) = semantic_name(code)?;
                    for (lane, name) in LANES.iter().enumerate().take(usize::from(size)) {
                        let value = format!("i{element}.{name}");
                        let value = if integer { value } else { format!("asuint({value})") };
                        values.push((vgpr + lane as u8, value));
                    }
                }
            }
            Interface::Pixel { input_addr, .. } => {
                // SPI_PS_INPUT_ADDR order: perspective and linear barycentrics, line
                // stipple, then position, front face, ancillary, coverage, fixed position.
                const SIZES: [u8; 16] = [2, 2, 2, 3, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1];
                let mut vgpr = 0u8;
                for (bit, size) in SIZES.iter().enumerate() {
                    if input_addr >> bit & 1 == 0 {
                        continue;
                    }
                    match bit {
                        0..=7 => {}
                        8..=11 => values.push((vgpr, format!("asuint(position.{})", LANES[bit - 8]))),
                        12 => values.push((vgpr, "asuint(front ? 1.0f : -1.0f)".to_owned())),
                        _ => bail!("Unsupported pixel system input {bit}"),
                    }
                    vgpr += size;
                }
            }
        }
        for (vgpr, value) in values {
            let target = self.v(vgpr);
            self.line(format!("{target} = {value};"));
        }
        Ok(())
    }

    fn sopp(&mut self, at: usize, op: u8, imm: u16) -> Result<()> {
        let condition = match op {
            // s_nop, s_endpgm, s_barrier, s_waitcnt, s_setprio.
            0 | 1 | 10 | 12 | 15 => return Ok(()),
            2 => "true",
            4 => "!scc",
            5 => "scc",
            6 => "!vcc",
            7 => "vcc",
            8 => "!exec",
            9 => "exec",
            other => bail!("Unsupported GCN program-control opcode {other}"),
        };
        // SOPP is 4 bytes: the offset counts dwords from the next instruction.
        let target = usize::try_from(at as i64 + 4 + i64::from(imm as i16) * 4)?;
        ensure!(target > at, "GCN program has a backward branch (loop)");
        let flag = format!("b{}", self.branches);
        self.branches += 1;
        let guard = self
            .guard(false)
            .map_or(String::new(), |guard| format!("({guard}) && "));
        self.line(format!("bool {flag} = {guard}{condition};"));
        self.skips.push((flag, target));
        Ok(())
    }

    /// Copy a descriptor register (pair), if the source holds one.
    fn copy_descriptor(&mut self, dst: u8, src: Src, pair: bool) -> bool {
        let Src::Sgpr(r) = src else { return false };
        let Some(desc) = self.sgpr.get(&r).copied() else {
            return false;
        };
        self.sgpr.insert(dst, desc);
        if pair && let Some(high) = self.sgpr.get(&(r + 1)).copied() {
            self.sgpr.insert(dst + 1, high);
        }
        true
    }

    fn sop1(&mut self, op: u8, dst: u8, src: Src) -> Result<()> {
        match op {
            3 => {
                // M0 (the interpolation primitive mask) and the compiler's VCC_HI tag
                // carry nothing the lift reads.
                if !self.copy_descriptor(dst, src, false) && !matches!(dst, 107 | 124) {
                    let value = self.src(src)?;
                    self.write_sgpr(dst, value)?;
                }
            }
            4 => {
                if !self.copy_descriptor(dst, src, true) {
                    let value = self.mask(src)?;
                    let target = self.mask_target(dst);
                    self.assign(target, value, false);
                }
            }
            10 => {
                ensure!(dst == 126, "Unsupported GCN whole-quad mode target");
                self.assign("exec".to_owned(), "true".to_owned(), false);
            }
            33 => ensure!(
                matches!(src, Src::Sgpr(r) if self.sgpr.get(&r) == Some(&Desc::FetchShader)),
                "GCN subroutine call is not the fetch shader"
            ),
            36 | 37 | 39 => {
                let source = self.mask(src)?;
                let target = self.mask_target(dst);
                self.assign(target, "exec".to_owned(), false);
                let value = match op {
                    36 => format!("({source} && exec)"),
                    37 => format!("({source} || exec)"),
                    _ => format!("({source} && !exec)"),
                };
                self.assign("exec".to_owned(), value, false);
                self.assign("scc".to_owned(), "exec".to_owned(), false);
            }
            other => bail!("Unsupported GCN scalar opcode SOP1 {other}"),
        }
        Ok(())
    }

    fn sop2(&mut self, op: u8, dst: u8, src: [Src; 2]) -> Result<()> {
        if matches!(op, 15 | 17 | 19 | 21 | 23) {
            let (a, b) = (self.mask(src[0])?, self.mask(src[1])?);
            let value = match op {
                15 => format!("({a} && {b})"),
                17 => format!("({a} || {b})"),
                19 => format!("({a} != {b})"),
                21 => format!("({a} && !{b})"),
                _ => format!("({a} || !{b})"),
            };
            let target = self.mask_target(dst);
            self.assign(target.clone(), value, false);
            self.assign("scc".to_owned(), target, false);
            return Ok(());
        }
        let (a, b) = (self.src(src[0])?, self.src(src[1])?);
        let value = match op {
            0 | 2 => format!("({a} + {b})"),
            1 | 3 => format!("({a} - {b})"),
            14 => format!("({a} & {b})"),
            16 => format!("({a} | {b})"),
            18 => format!("({a} ^ {b})"),
            30 => format!("({a} << ({b} & 31u))"),
            32 => format!("({a} >> ({b} & 31u))"),
            38 => format!("({a} * {b})"),
            other => bail!("Unsupported GCN scalar opcode SOP2 {other}"),
        };
        self.write_sgpr(dst, value)
    }

    fn sopc(&mut self, op: u8, src: [Src; 2]) -> Result<()> {
        ensure!(op < 12, "Unsupported GCN scalar compare {op}");
        let (mut a, mut b) = (self.src(src[0])?, self.src(src[1])?);
        if op < 6 {
            a = format!("asint({a})");
            b = format!("asint({b})");
        }
        let relation = ["==", "!=", ">", ">=", "<", "<="][usize::from(op % 6)];
        self.assign("scc".to_owned(), format!("({a} {relation} {b})"), false);
        Ok(())
    }

    fn sopk(&mut self, op: u8, dst: u8, imm: u16) -> Result<()> {
        let imm = imm as i16 as i32 as u32;
        let value = match op {
            0 => format!("{imm}u"),
            15 => format!("({} + {imm}u)", self.s(dst)),
            other => bail!("Unsupported GCN scalar opcode SOPK {other}"),
        };
        self.write_sgpr(dst, value)
    }

    fn step(&mut self, at: usize, op: &Op) -> Result<()> {
        self.skips.retain(|(_, end)| *end > at);
        match op {
            Op::Sopp { op, imm } => self.sopp(at, *op, *imm),
            Op::Sop1 { op, dst, src } => self.sop1(*op, *dst, *src),
            Op::Sop2 { op, dst, src } => self.sop2(*op, *dst, *src),
            Op::Sopc { op, src } => self.sopc(*op, *src),
            Op::Sopk { op, dst, imm } => self.sopk(*op, *dst, *imm),
            Op::Smrd {
                op,
                dst,
                base,
                offset,
            } => self.scalar_load(*op, *dst, *base, *offset),
            Op::Valu(valu) => {
                ensure!(
                    !matches!(valu.op, 257 | 258 | 386),
                    "GCN program reads across lanes"
                );
                self.valu(valu)
            }
            Op::Vintrp {
                op,
                dst,
                attr,
                channel,
            } => {
                ensure!(*op < 2, "Unsupported GCN interpolation {op}");
                let target = self.v(*dst);
                let value = format!("asuint(a{attr}.{})", LANES[usize::from(*channel)]);
                self.assign(target, value, true);
                Ok(())
            }
            Op::Ds { op } => bail!("GCN program uses local data share opcode {op}"),
            Op::Mtbuf(buffer) => self.typed_load(buffer),
            Op::Mubuf(buffer) => bail!("Unsupported GCN buffer opcode {}", buffer.op),
            Op::Mimg(image) => self.image(image),
            Op::Exp(export) => self.export(export),
        }
    }

    /// The entry point's parameters, and the outputs it clears first.
    fn parameters(
        &self,
        elements: &[(u8, u8)],
        linkage: &[u8],
    ) -> Result<(Vec<String>, Vec<String>)> {
        let mut parameters = Vec::new();
        let mut outputs = Vec::new();
        match &self.header.interface {
            Interface::Vertex { inputs, exports } => {
                let mut sorted = inputs.clone();
                sorted.sort_unstable();
                for (element, _, size) in sorted {
                    let (code, index) = elements[usize::from(element)];
                    let (name, integer) = semantic_name(code)?;
                    let ty = if integer { "uint" } else { "float" };
                    parameters.push(format!("in {ty}{size} i{element} : {name}{index}"));
                }
                let mut params = exports.clone();
                params.sort_unstable_by_key(|(_, param)| *param);
                for (semantic, param) in &params {
                    parameters.push(format!("out float4 o{param} : TEXCOORD{semantic}"));
                    outputs.push(format!("o{param}"));
                }
                parameters.push("out float4 position : SV_Position".to_owned());
                outputs.push("position".to_owned());
                ensure!(self.position, "GCN vertex program exports no position");
            }
            Interface::Pixel { inputs, .. } => {
                parameters.extend(interpolants(inputs, linkage)?);
                parameters.push("in float4 position : SV_Position".to_owned());
                parameters.push("in bool front : SV_IsFrontFace".to_owned());
                ensure!(!self.targets.is_empty(), "GCN pixel program exports no colour");
                for target in &self.targets {
                    parameters.push(format!("out float4 o{target} : SV_Target{target}"));
                    outputs.push(format!("o{target}"));
                }
            }
        }
        Ok((parameters, outputs))
    }

    fn declarations(&self) -> Result<String> {
        let mut text = String::new();
        for (slot, rows) in &self.cbuffers {
            let rows = match rows {
                Some(rows) => *rows,
                // D1 bones: the verified 72-bone palette, three rows each.
                None if *slot == 10 => 216,
                None => 4096,
            };
            writeln!(text, "cbuffer CB{slot} : register(b{slot}) {{ float4 cb{slot}[{rows}]; }}")?;
        }
        for slot in &self.textures {
            let kind = if self.cubes.contains(slot) { "TextureCube" } else { "Texture2D" };
            writeln!(text, "{kind}<float4> tex{slot} : register(t{slot});")?;
        }
        for slot in &self.samplers {
            writeln!(text, "SamplerState smp{slot} : register(s{slot});")?;
        }
        text.push_str(concat!(
            "float mul_legacy(float a, float b) { return (a == 0.0f || b == 0.0f) ? 0.0f : a * b; }\n",
            "float rsq_clamp(float a) { return min(rsqrt(a), 3.402823466e38f); }\n",
            "// GCN v_cube* (major axis z, then y, then x, ties to the later axis).\n",
            "uint cube_axis(float3 d) { float3 a = abs(d); return a.z >= a.x && a.z >= a.y ? 2u : (a.y >= a.x ? 1u : 0u); }\n",
            "float cube_id(float3 d) { uint m = cube_axis(d); return (float)(m * 2u + (d[m] < 0.0f ? 1u : 0u)); }\n",
            "float cube_sc(float3 d) { uint m = cube_axis(d); return m == 2u ? (d.z < 0.0f ? -d.x : d.x) : (m == 1u ? d.x : (d.x < 0.0f ? d.z : -d.z)); }\n",
            "float cube_tc(float3 d) { uint m = cube_axis(d); return m == 1u ? (d.y < 0.0f ? -d.z : d.z) : -d.y; }\n",
            "float cube_ma(float3 d) { return 2.0f * d[cube_axis(d)]; }\n",
            "// The direction a face and its 1.5-offset coordinates address (D3D cube faces).\n",
            "float3 cube_dir(float s, float t, float face) {\n",
            "    float u = (s - 1.5f) * 2.0f, v = (t - 1.5f) * 2.0f;\n",
            "    switch ((uint)round(face)) {\n",
            "    case 0u: return float3(1.0f, -v, -u);\n",
            "    case 1u: return float3(-1.0f, -v, u);\n",
            "    case 2u: return float3(u, 1.0f, v);\n",
            "    case 3u: return float3(u, -1.0f, -v);\n",
            "    case 4u: return float3(u, -v, 1.0f);\n",
            "    default: return float3(-u, -v, -1.0f);\n",
            "    }\n",
            "}\n",
        ));
        Ok(text)
    }
}

/// A pixel program's interpolants, declared as the full vertex export list so every
/// interpolant keeps the register the vertex program writes it to.
fn interpolants(inputs: &[u16], linkage: &[u8]) -> Result<Vec<String>> {
    for (attribute, input) in inputs.iter().enumerate() {
        ensure!(
            linkage.contains(&((*input & 0xFF) as u8)),
            "Pixel input {attribute} has no vertex export"
        );
    }
    linkage
        .iter()
        .map(|semantic| {
            let Some(attribute) = inputs
                .iter()
                .position(|input| (*input & 0xFF) as u8 == *semantic)
            else {
                return Ok(format!("in float4 unused{semantic} : TEXCOORD{semantic}"));
            };
            // GnmPixelInputSemantic: flat bit 10, linear bit 11, custom bit 12.
            let flags = inputs[attribute];
            ensure!(flags & 0x1000 == 0, "Unsupported custom pixel interpolant");
            let mode = if flags & 0x400 != 0 {
                "nointerpolation "
            } else if flags & 0x800 != 0 {
                "noperspective "
            } else {
                ""
            };
            Ok(format!("in {mode}float4 a{attribute} : TEXCOORD{semantic}"))
        })
        .collect()
}

/// Lift a program. `elements` are the native layout's (semantic, index) pairs in stream
/// order, for a vertex program; `linkage` the vertex program's export semantics in
/// parameter order, which a pixel program declares in full to keep the same registers.
pub fn hlsl(
    header: &Header,
    code: &[u8],
    elements: &[(u8, u8)],
    linkage: &[u8],
    cubes: &BTreeSet<u8>,
) -> Result<String> {
    let instructions = decode::program(code)?;
    let mut lift = Lift::new(header, cubes)?;
    lift.prologue(elements)?;
    for Instruction { at, op } in &instructions {
        lift.step(*at, op)?;
    }
    let (parameters, outputs) = lift.parameters(elements, linkage)?;
    let mut text = lift.declarations()?;
    writeln!(text, "void main({})\n{{", parameters.join(",\n    "))?;
    text.push_str("    bool exec = true, vcc = false, scc = false;\n");
    for name in outputs {
        writeln!(text, "    {name} = 0;")?;
    }
    for r in &lift.masks {
        writeln!(text, "    bool km{r} = false;")?;
    }
    for r in &lift.sgprs {
        writeln!(text, "    uint sg{r} = 0;")?;
    }
    for r in &lift.vgprs {
        writeln!(text, "    uint vg{r} = 0;")?;
    }
    text.push_str(&lift.body);
    text.push_str("}\n");
    Ok(text)
}
