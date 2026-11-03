use super::*;
use crate::tiger::shader::{
    identity::Stage,
    program::{Program, Template},
};

/// How a gbuffer-pass pixel program reaches the native targets.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Gbuffer {
    /// A surface writes all three native targets through the deferred adapter.
    Surface,
    /// A decal keeps its source targets. `replace` marks an unblended source state.
    Decal { replace: bool },
}

pub(super) struct Shader {
    pub stage: Stage,
    pub gbuffer: Option<Gbuffer>,
    pub layout: u16,
    pub depth: Option<super::super::expression::Depth>,
    /// The texture register of the reticle mask a pixel program loads.
    pub reticle_mask: Option<u32>,
    /// SM5 bytecode lifted from a PS4 GCN program, which replaces the source program, and
    /// the symbol suffix naming what the lift depended on.
    pub lifted: Option<(Vec<u8>, String)>,
}

impl Shader {
    /// Stored layouts without a per-vertex detail scale that a source vertex
    /// program may still name: weighted 9 and unweighted 123.
    fn no_detail_scale(&self) -> bool {
        self.stage == Stage::Vertex && matches!(self.layout, 9 | 123)
    }

    fn symbol(&self, tag: u32) -> String {
        let mut symbol = format!(
            "shader-{tag:08X}-{}",
            match self.gbuffer {
                Some(Gbuffer::Surface) => "deferred",
                Some(Gbuffer::Decal { replace: false }) => "decal",
                Some(Gbuffer::Decal { replace: true }) => "decal-replace",
                None => "source",
            }
        );
        if self.no_detail_scale() {
            symbol.push_str(&format!("-layout-{}", self.layout));
        }
        if let Some(depth) = self.depth {
            symbol.push_str(&format!("-depth-{}-{}", depth.source, depth.native));
        }
        if let Some(slot) = self.reticle_mask {
            symbol.push_str(&format!("-reticle-{slot}"));
        }
        if let Some((_, suffix)) = &self.lifted {
            symbol.push_str(&format!("-gcn{suffix}"));
        }
        symbol
    }
}

/// The D3D11 sampler description (`D3D11_SAMPLER_DESC`, 52 bytes) of a GNM sampler, by
/// freegnm's `GnmSampler` (`include/gnm/sampler.h`) and its enums (`types.h`).
fn gnm_sampler(gnm: [u8; 16]) -> [u8; 52] {
    let w = |i: usize| u32::from_le_bytes(gnm[i * 4..i * 4 + 4].try_into().unwrap());
    let (w0, w1, w2, w3) = (w(0), w(1), w(2), w(3));
    // GnmTexClamp: wrap, mirror, clamp to last texel, mirror once to last texel, clamp to
    // half border, mirror once to half border, clamp to border, mirror once to border.
    let address = |clamp: u32| -> u32 { [1, 2, 3, 5, 4, 5, 4, 5][clamp as usize & 7] };
    let compare = (w0 >> 12) & 7;
    let (magnify, minify, mip) = ((w2 >> 20) & 3, (w2 >> 22) & 3, (w2 >> 26) & 3);
    let mut filter = if magnify >= 2 || minify >= 2 {
        0x55
    } else {
        (u32::from(minify == 1) << 4) | (u32::from(magnify == 1) << 2) | u32::from(mip == 2)
    };
    if compare != 0 {
        filter |= 0x80;
    }
    // LOD bias is signed 6.8 fixed point; minimum and maximum LOD unsigned 4.8.
    let bias = (((w2 & 0x3FFF) << 18) as i32 >> 18) as f32 / 256.0;
    let border = match w3 >> 30 {
        1 => [0.0, 0.0, 0.0, 1.0],
        2 => [1.0; 4],
        _ => [0.0; 4],
    };
    let mut out = Vec::with_capacity(52);
    for value in [filter, address(w0), address(w0 >> 3), address(w0 >> 6)] {
        out.extend(value.to_le_bytes());
    }
    out.extend(bias.to_le_bytes());
    out.extend((1u32 << ((w0 >> 9) & 7)).min(16).to_le_bytes());
    out.extend(if compare == 0 { 8u32 } else { compare + 1 }.to_le_bytes());
    for lane in border {
        out.extend(f32::to_le_bytes(lane));
    }
    out.extend(((w1 & 0xFFF) as f32 / 256.0).to_le_bytes());
    out.extend((((w1 >> 12) & 0xFFF) as f32 / 256.0).to_le_bytes());
    out.try_into().unwrap()
}

impl Assembly<'_> {
    pub(super) fn texture(&mut self, tag: u32) -> Result<String> {
        let symbol = format!("texture-{tag:08X}");
        if self.symbols.contains(&symbol) {
            return Ok(symbol);
        }
        let texture = self
            .source
            .linear_texture(tag, self.options.xg_library.as_deref())?;
        self.texture_bytes(&symbol, texture)
    }

    pub(super) fn texture_bytes(
        &mut self,
        symbol: &str,
        texture: super::super::texture::Texture,
    ) -> Result<String> {
        let subtype = if texture.depth > 1 {
            3
        } else if texture.layers == 6 {
            2
        } else {
            1
        };
        let template = self.template(32, subtype)?;
        let data_template = self.template(40, subtype)?;
        // Preserve a checked native volume descriptor's topology flags. These are not the
        // cube flag used by 2D resources, and a subtype alone does not establish its shape.
        let mut header = if texture.depth > 1 {
            let native = self.native.tag(template, None)?;
            ensure!(
                native.0.len() == 40
                    && native.u16(12)? == 0xCAFE
                    && native.u16(18)? > 1
                    && native.u16(20)? == 1,
                "Native volume texture template differs"
            );
            native.0.clone()
        } else {
            vec![0; 40]
        };
        put(
            &mut header,
            0,
            &u32::try_from(texture.bytes.len())?.to_le_bytes(),
        )?;
        put(&mut header, 4, &texture.format.to_le_bytes())?;
        put(&mut header, 12, &0xCAFEu16.to_le_bytes())?;
        for (at, value) in [
            (14, texture.width),
            (16, texture.height),
            (18, texture.depth),
            (20, texture.layers),
        ] {
            put(&mut header, at, &value.to_le_bytes())?;
        }
        // A 4x4 footprint includes one complete BC block and every uncompressed pixel.
        header[22] = u8::try_from(super::super::texture::surface(texture.format, 4, 4)?.1 / 2)?;
        header[23] = texture.mips;
        if texture.depth == 1 {
            header[25] = 1;
            header[31] = 1;
            header[32] = if texture.layers == 6 { 2 } else { 0 };
            header[33] = 3;
            header[34] = 1;
        }
        put(&mut header, 36, &u32::MAX.to_le_bytes())?;
        crate::tiger::texture::resident(&mut header, texture.bytes.len())?;
        let data = format!("{symbol}-data");
        self.node(&data, data_template, &texture.bytes, Some(symbol), &[])?;
        self.node(symbol, template, &header, Some(&data), &[])
    }

    pub(super) fn sampler(&mut self, tag: u32) -> Result<String> {
        let symbol = format!("sampler-{tag:08X}");
        if self.symbols.contains(&symbol) {
            return Ok(symbol);
        }
        let header = self.source.read(tag)?;
        let descriptor = if self.source.platform() == 7 {
            // PS4 keeps its 16-byte GNM sampler inside the 24-byte header.
            ensure!(
                header.0.len() == 24
                    && header.u64(0)? == 24
                    && self.source.reference(tag)? == 0x80801A42,
                "Unknown PS4 source sampler storage"
            );
            Payload(gnm_sampler(header.0[8..24].try_into().unwrap()).to_vec())
        } else {
            let descriptor = self.source.read(self.source.reference(tag)?)?;
            ensure!(
                header.0 == [0; 8] && descriptor.0.len() == 60 && descriptor.0[52..] == [255; 8],
                "Unknown source sampler storage"
            );
            descriptor
        };
        let template = self.template(34, 1)?;
        let native_header = self.native.tag(template, None)?;
        let data_template = self.native.reference(template)?;
        let native_descriptor = self.native.tag(data_template, None)?;
        ensure!(
            native_descriptor.0.len() == 52,
            "Native sampler descriptor extent differs"
        );
        let data = format!("{symbol}-data");
        self.node(
            &data,
            data_template,
            &descriptor.0[..52],
            Some(&symbol),
            &[],
        )?;
        self.node(&symbol, template, &native_header.0, Some(&data), &[])
    }

    /// A source program's SM5 bytecode: lifted from PS4 GCN, or stored in the program.
    fn program_bytes(&self, tag: u32, lifted: Option<(Vec<u8>, String)>) -> Result<Vec<u8>> {
        if let Some((code, _)) = lifted {
            return Ok(code);
        }
        let source = self.source.read(tag)?;
        let range = source.array_range(8, 1, Some(0x80800009))?;
        Ok(source.0.get(range).context("Source GPU program")?.to_vec())
    }

    pub(super) fn shader(&mut self, tag: u32, inputs: Shader) -> Result<String> {
        let symbol = inputs.symbol(tag);
        let no_detail_scale = inputs.no_detail_scale();
        let Shader {
            stage,
            gbuffer,
            layout,
            depth,
            reticle_mask,
            lifted,
        } = inputs;
        if self.symbols.contains(&symbol) {
            return Ok(symbol);
        }
        let raw = &self.program_bytes(tag, lifted)?;
        if self.bone_map.is_some() && stage == Stage::Vertex {
            super::super::shader::rigid_indices(raw)
                .with_context(|| format!("Source mapped vertex program {tag:08X}"))?;
        }
        let mut code = if stage == Stage::Vertex {
            super::super::shader::skinning(raw)
                .with_context(|| format!("Source vertex program {tag:08X}"))?
        } else if gbuffer == Some(Gbuffer::Surface) {
            super::super::shader::deferred(raw, &self.glow)
                .with_context(|| format!("Source deferred program {tag:08X}"))?
        } else if let Some(Gbuffer::Decal { replace }) = gbuffer {
            super::super::shader::decal(raw, replace)
                .with_context(|| format!("Source decal program {tag:08X}"))?
        } else {
            super::super::shader::compact(raw)
                .with_context(|| format!("Source GPU program {tag:08X}"))?
        };
        if no_detail_scale {
            let specialized = super::super::shader::without_detail_scale(&code, layout)
                .with_context(|| format!("Source vertex program {tag:08X}, layout {layout}"))?;
            if specialized.approximated {
                // The absent input has no established source value. The import
                // reports the adopted base-UV convention instead of a guess.
                self.evidence
                    .push(json!({"shader":tag,"symbol":symbol,"layout":layout,
                    "approximation":"missing_detail_scale_uses_base_uv"}));
                self.approximations.insert(format!(
                    "Vertex program {tag:08X} reads a per-vertex detail scale that its layout {layout} draws do not store. Those draws use the base texture coordinates for the detail pair, following the source convention for that layout. Their detail texture scale is approximated."
                ));
            }
            code = specialized.code;
        }
        if let Some(depth) = depth {
            code = super::super::shader::depth(&code, &depth)
                .with_context(|| format!("Source transparent depth program {tag:08X}"))?;
        }
        if let Some(slot) = reticle_mask {
            code = super::super::shader::reticle_mask(&code, slot)
                .with_context(|| format!("Source sight program {tag:08X}"))?;
        }
        let template_tag = self.template(
            33,
            if matches!(stage, Stage::Hull | Stage::Domain) {
                1
            } else {
                stage.subtype()
            },
        )?;
        let template = Template::read(&mut self.native, template_tag)?;
        let program = Program::new(stage, code)?;
        let data = format!("{symbol}-bytecode");
        if matches!(stage, Stage::Hull | Stage::Domain) {
            self.node(
                &data,
                template.bytecode_tag(),
                program.bytecode(),
                Some(&symbol),
                &[],
            )?;
            self.node(
                &symbol,
                template.header_tag(),
                &program.header().0,
                Some(&data),
                &[],
            )?;
            let storage = if stage == Stage::Hull {
                "gpu_hull"
            } else {
                "gpu_domain"
            };
            for node in self.nodes.iter_mut().rev().take(2) {
                node["storage"] = json!(storage);
            }
            Ok(symbol)
        } else {
            let assets = program.assets(&symbol, &template)?;
            self.node(
                &data,
                assets.bytecode_template,
                &assets.bytecode,
                Some(&symbol),
                &[],
            )?;
            self.node(
                &symbol,
                assets.header_template,
                &assets.header,
                Some(&data),
                &[],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::gnm_sampler;

    fn field(desc: &[u8; 52], at: usize) -> u32 {
        u32::from_le_bytes(desc[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn gnm_samplers_become_d3d11_descriptions() {
        // PS4 samplers 80AAE177 (trilinear wrap) and 80AADBAB (4x anisotropic clamp).
        let trilinear = gnm_sampler([
            0, 0, 0, 0, 0, 0, 0xF0, 0, 0x80, 0x3F, 0x50, 0x0A, 0, 0, 0, 0,
        ]);
        assert_eq!(field(&trilinear, 0), 0x15);
        assert_eq!([4, 8, 12].map(|at| field(&trilinear, at)), [1, 1, 1]);
        assert_eq!(f32::from_bits(field(&trilinear, 16)), -0.5);
        assert_eq!(field(&trilinear, 20), 1);
        assert_eq!(f32::from_bits(field(&trilinear, 48)), 15.0);
        let anisotropic = gnm_sampler([
            0x92, 0x04, 0, 0, 0, 0, 0xF0, 0, 0x80, 0x3F, 0xF0, 0x0A, 0, 0, 0, 0,
        ]);
        assert_eq!(field(&anisotropic, 0), 0x55);
        assert_eq!([4, 8, 12].map(|at| field(&anisotropic, at)), [3, 3, 3]);
        assert_eq!(field(&anisotropic, 20), 4);
    }
}
