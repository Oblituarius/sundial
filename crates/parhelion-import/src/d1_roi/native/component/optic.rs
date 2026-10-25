//! Translate a checked source optic and both of its movement providers.
use super::*;
mod fallback;

const OPTIC: u32 = 0x81529E1F;
const ENTITY: u32 = 0x80BBCC85;
const ADAPTER: u32 = 0x80C70C06;
/// The native optic's only numeric provider: interface `80809AE1`, method 2 of `80803941`.
/// In stock entity `8161ECA5` it feeds bank receiver `4F437B90`, the name D1 sight banks
/// receive from their optic's only numeric provider (`8080053B`, method 3 of `80802170`).
/// Both optics list their six interfaces in the same order and kinds.
const VALUE_PROVIDER: u64 = 0x598;

/// The native connection feeding a bank receiver declaration from the optic's value provider.
pub(super) fn receiver_connection(
    optic: &Payload,
    [bank_id, optic_id]: [u32; 2],
    declaration: usize,
    [bank_index, optic_index]: [usize; 2],
    native: &mut Reader,
) -> Result<Vec<u8>> {
    use crate::tiger::entity::links;
    let metadata = native.tag(optic.u32(VALUE_PROVIDER as usize + 8)?, Some(0x80809C54))?;
    let object = links::Object {
        owner: OPTIC,
        class: 0x80809AE1,
        offset: VALUE_PROVIDER,
    };
    let interface = links::Interface::read(optic, object, &metadata, false)?;
    ensure!(
        interface.instance_class == 0x80809AE2
            && interface.methods.len() == 1
            && interface.methods[0].implementation_class == 0x80803941
            && interface.methods[0].index == 2,
        "Native optic value provider differs"
    );
    let mut row = vec![0; 72];
    put(&mut row, 0, &0x811C9DC5u32.to_le_bytes())?;
    put(&mut row, 8, &bank_id.to_le_bytes())?;
    put(&mut row, 12, &0x808097A8u32.to_le_bytes())?;
    put(&mut row, 16, &(declaration as u64).to_le_bytes())?;
    put(&mut row, 24, &(bank_index as u64).to_le_bytes())?;
    put(&mut row, 32, &0x811C9DC5u32.to_le_bytes())?;
    put(&mut row, 40, &optic_id.to_le_bytes())?;
    put(&mut row, 44, &0x80809AE1u32.to_le_bytes())?;
    put(&mut row, 48, &VALUE_PROVIDER.to_le_bytes())?;
    put(&mut row, 56, &(optic_index as u64).to_le_bytes())?;
    Ok(row)
}

impl Assembly<'_> {
    /// Bank receivers a sight of this entity feeds through its value provider, keyed by bank
    /// and source declaration. Other providers have no translation, so their receivers are
    /// absent here.
    pub(super) fn optic_feeds(&mut self, entity: &Payload) -> Result<BTreeMap<(u32, u64), u32>> {
        let components = entity
            .array(0x20, 12, Some(0x80800715))?
            .into_iter()
            .map(|at| entity.u32(at))
            .collect::<Result<Vec<_>>>()?;
        let mut feeds = BTreeMap::new();
        for row in entity.array(0x30, 88, Some(0x8080054E))? {
            let selector = usize::try_from(entity.u64(row + 72)?)?;
            let Some(&tag) = components.get(selector) else {
                continue;
            };
            if entity.u64(row + 24)? != 0x80800668
                || entity.u64(row + 40)? != 0x811C9DC5
                || entity.u64(row + 48)? != u64::from(tag)
                || entity.u64(row + 64)? != 0x8080053B
                || entity.u64(row + 80)? != 0
            {
                continue;
            }
            let optic = self.source.read(tag)?;
            if optic.u32(optic.pointer(24)? - 4)? != 0x808027FC {
                continue;
            }
            let offset = usize::try_from(entity.u64(row + 56)?)?;
            let metadata = self.source.read(optic.u32(offset + 8)?)?;
            let methods = metadata.array(16, 24, Some(0x808005C0))?;
            if metadata.u32(8)? == 0x8080053B
                && metadata.u32(12)? == 0x80800539
                && methods.len() == 1
                && metadata.u32(methods[0])? == 0x80802170
                && metadata.u32(methods[0] + 4)? == 3
                && metadata.bytes::<16>(methods[0] + 8)? == [0; 16]
            {
                feeds.insert((entity.u32(row + 8)?, entity.u64(row + 16)?), tag);
            }
        }
        Ok(feeds)
    }

    fn aim_adapter(&mut self, tag: u32, offset: u64) -> Result<()> {
        let source = self.source.read(tag)?;
        let native = self.native.tag(ADAPTER, Some(0x80809C36))?;
        ensure!(
            offset == 0xA8,
            "Source optic adapter interface offset differs"
        );
        for (owner, source_format) in [(&source, true), (&*native, false)] {
            let definition = owner.pointer(24)?;
            let class = if source_format {
                0x80800346
            } else {
                0x80809783
            };
            ensure!(
                owner.u32(definition - 4)? == class && owner.pointer(0xA8)? == definition,
                "Optic movement adapter definition differs"
            );
            let metadata = if source_format {
                self.source.read(owner.u32(0xB0)?)?
            } else {
                (*self.native.tag(owner.u32(0xB0)?, None)?).clone()
            };
            let (interface, instance, implementation, row_class) = if source_format {
                (0x80800921, 0x8080052A, 0x80800801, 0x808005C0)
            } else {
                (0x80809BE3, 0x80809BE2, 0x80809782, 0x80809C56)
            };
            ensure!(
                metadata.u32(8)? == interface && metadata.u32(12)? == instance,
                "Optic movement adapter dispatch differs"
            );
            let methods = metadata.array(16, 24, Some(row_class))?;
            ensure!(
                methods.len() == 4,
                "Optic movement adapter method count differs"
            );
            for (at, method) in methods.into_iter().zip(9..13) {
                ensure!(
                    metadata.u32(at)? == implementation
                        && metadata.u32(at + 4)? == method
                        && metadata.bytes::<16>(at + 8)? == [0; 16],
                    "Optic movement adapter method differs"
                );
            }
        }
        Ok(())
    }

    pub(super) fn optic(
        &mut self,
        source_tag: u32,
        source: &Payload,
        source_entity: &Payload,
        name: &str,
        index: usize,
    ) -> Result<Component> {
        let mut component = self.component(OPTIC, ENTITY, name, index)?;
        let s = source.pointer(24)?;
        let n = component.payload.pointer(24)?;
        activation(source, &mut component.payload)?;
        ensure!(
            source.u32(s - 4)? == 0x808027FC && component.payload.u32(n - 4)? == 0x8080393B,
            "Optic resource layout differs"
        );
        ensure!(
            source.u64(s + 0x70)? == 0 && source.u64(s + 0x78)? <= 1,
            "Source optic camera flags differ"
        );
        put(
            &mut component.payload.0,
            n + 0x60,
            &source.bytes::<8>(s + 0x78)?,
        )?;
        for (from, to) in [(0x80, 0x70), (0x90, 0x80)] {
            for at in (0..16).step_by(4) {
                ensure!(
                    source.f32(s + from + at)?.is_finite(),
                    "Nonfinite source optic camera value"
                );
            }
            put(
                &mut component.payload.0,
                n + to,
                &source.bytes::<16>(s + from)?,
            )?;
        }
        let code = |p: &Payload, at| -> Result<Vec<u8>> {
            p.array(at, 1, Some(0x80800009))?
                .into_iter()
                .map(|at| p.u8(at))
                .collect()
        };
        let zoom = code(source, s + 0xB8)?;
        ensure!(
            source.bytes::<16>(s + 0xD8)? == component.payload.bytes::<16>(n + 0xC0)?,
            "Source optic zoom parameters differ"
        );
        let constants = source.array(s + 0xC8, 16, Some(0x80800090))?;
        let mut values = Vec::new();
        let mut lanes = Vec::new();
        for from in constants {
            let mut vector = [0; 4];
            for (lane, bits) in vector.iter_mut().enumerate() {
                *bits = source.u32(from + lane * 4)?;
                ensure!(
                    f32::from_bits(*bits).is_finite(),
                    "Nonfinite optic zoom constant"
                );
            }
            lanes.push(vector);
            values.extend(source.bytes::<16>(from)?);
        }
        // Optic procedures use the channel dialect. Preserve the source curve instead of
        // requiring the stock optic template to contain the same expression and vector count.
        crate::d1_roi::procedure::validate(&zoom, &lanes, source.u64(s + 0xD8)? as usize)?;
        array(&mut component.payload.0, n + 0xA0, 0x80800009, &zoom, 1)?;
        array(&mut component.payload.0, n + 0xB0, 0x80800090, &values, 16)?;
        put(
            &mut component.payload.0,
            n + 0xE0,
            &source.bytes::<24>(s + 0xF8)?,
        )?;
        // Input definitions `80800927` are 40 bytes with the name at +32 on PS4, as material
        // input definitions are.
        let inputs = source.array(s + 0xE8, self.source.field(48, 40), Some(0x80800927))?;
        let definitions = component.payload.array(n + 0xD0, 40, Some(0x80809789))?;
        ensure!(
            inputs.len() == definitions.len(),
            "Source optic zoom input count differs"
        );
        for (from, to) in inputs.into_iter().zip(definitions) {
            let name = source.u32(from + self.source.field(40, 32))?;
            ensure!(
                name == component.payload.u32(to + 32)?,
                "Source optic zoom input identity differs"
            );
            component.inputs.push((name, to));
        }
        let source_connections = source_entity.array(0x30, 88, Some(0x8080054E))?;
        let native_connections = component.entity.array(0x20, 72, Some(0x80809BC9))?;
        // PS4 provider objects are 8 bytes (first) and 32 bytes (second) shorter, start 0x40
        // before their poses and hold the instance class as a u32 after a zero word, where
        // Xbox One holds it as a u64 (measured on CUSA00219 optic `80A77047`).
        let ps4 = self.source.platform() == 7;
        let aims = [
            (self.source.field(0x1D0, 0x1C8), 0x198, 0x8D0DD3CD),
            (self.source.field(0x270, 0x250), 0x228, 0x0B7BA45D),
        ];
        let (class_at, padding_at) = if ps4 { (36, 40) } else { (40, 36) };
        for (sd, nd, marker) in aims {
            ensure!(
                source.u32(s + sd - class_at)? == 0x8080052A
                    && source.u32(s + sd - padding_at)? == 0
                    && source.u32(s + sd - 32)? <= 2
                    && source.u32(s + sd - 28)? == u32::MAX
                    && source.bytes::<8>(s + sd - 24)? == [0; 8]
                    && source.u32(s + sd - 12)? == marker,
                "Source optic aim provider layout differs"
            );
            put(
                &mut component.payload.0,
                n + nd - 32,
                &source.bytes::<4>(s + sd - 32)?,
            )?;
            let poses = source.array(s + sd, 64, Some(0x80800608))?;
            ensure!(
                poses.len() <= 1,
                "Source optic has multiple default aim poses"
            );
            let inherited = if poses.is_empty() {
                ensure!(
                    source.u32(s + sd - 32)? == 0,
                    "Empty optic aim input has an unsupported selection mode"
                );
                Some(self.inherited_aim(marker)?)
            } else {
                None
            };
            let mut bytes = Vec::new();
            for at in poses {
                ensure!(
                    source.u32(at)? == 0
                        && source.u32(at + 4)? <= 1
                        && source.bytes::<8>(at + 8)? == [0; 8]
                        && source.u32(at + 48)? == marker,
                    "Source optic aim pose needs a different skeleton binding"
                );
                for offset in (16..48).step_by(4) {
                    ensure!(
                        source.f32(at + offset)?.is_finite(),
                        "Nonfinite source optic aim pose"
                    );
                }
                bytes.extend(source.bytes::<64>(at)?);
            }
            if let Some((owner, row)) = inherited {
                bytes.extend(row);
                put(&mut component.payload.0, n + nd - 32, &1u32.to_le_bytes())?;
                self.evidence.push(json!({"source_optic":source_tag,"inherited_aim_owner":owner,"marker":marker,"aim":"Source weapon default on the matching root skeleton"}));
            }
            array(&mut component.payload.0, n + nd, 0x80809C00, &bytes, 64)?;
            let from = source_connections
                .iter()
                .copied()
                .filter(|at| {
                    source_entity.u64(at + 8).ok() == Some(u64::from(source_tag))
                        && source_entity.u64(at + 16).ok()
                            == Some((s + sd - self.source.field(0x48, 0x40)) as u64)
                })
                .collect::<Vec<_>>();
            let to = native_connections
                .iter()
                .copied()
                .filter(|at| {
                    component.entity.u32(at + 8).ok() == Some(OPTIC)
                        && component.entity.u64(at + 16).ok() == Some((n + nd - 0x40) as u64)
                })
                .collect::<Vec<_>>();
            let ([from], [to]) = (from.as_slice(), to.as_slice()) else {
                anyhow::bail!("Optic aim connection is absent or ambiguous");
            };
            ensure!(
                source_entity.u64(from + 24)? == 0x8080073D && source_entity.u64(from + 80)? == 0,
                "Source optic aim input contract differs"
            );
            let namespace = source_entity.u64(from + 40)?;
            let selector = source_entity.u64(from + 72)?;
            let provider = source_entity.u64(from + 48)?;
            let offset = source_entity.u64(from + 56)?;
            let class = source_entity.u64(from + 64)?;
            let endpoint = match selector {
                0x3FFFF => {
                    ensure!(
                        namespace == 0xAE7D0AED
                            && provider == u64::from(u32::MAX)
                            && offset == 0
                            && class == u64::from(u32::MAX),
                        "Source optic attachment provider differs"
                    );
                    [u32::MAX, u32::MAX]
                }
                0x20000 => {
                    ensure!(
                        namespace == 0x811C9DC5 && class == 0x80800921,
                        "Source optic adapter provider differs"
                    );
                    self.aim_adapter(u32::try_from(provider)?, offset)?;
                    [ADAPTER, 0x80809BE3]
                }
                _ => anyhow::bail!("Unsupported source optic movement connection"),
            };
            put(&mut component.entity.0, to + 32, &namespace.to_le_bytes())?;
            put(&mut component.entity.0, to + 40, &endpoint[0].to_le_bytes())?;
            put(&mut component.entity.0, to + 44, &endpoint[1].to_le_bytes())?;
            put(&mut component.entity.0, to + 48, &offset.to_le_bytes())?;
            put(&mut component.entity.0, to + 56, &selector.to_le_bytes())?;
            if inherited.is_some() {
                // The selected art is attached to the weapon under this native namespace.
                // Binding zero keeps the inherited row in the matched root skeleton's space.
                put(
                    &mut component.entity.0,
                    to + 32,
                    &0xAE7D0AEDu64.to_le_bytes(),
                )?;
                put(&mut component.entity.0, to + 40, &[255; 8])?;
                put(&mut component.entity.0, to + 48, &0u64.to_le_bytes())?;
                put(&mut component.entity.0, to + 56, &0x3FFFFu64.to_le_bytes())?;
                put(&mut component.entity.0, to + 64, &0u64.to_le_bytes())?;
            }
        }
        component.seal()?;
        self.evidence
            .push(json!({"source_optic":source_tag,"native_template":OPTIC,
            "aim":"Source defaults, movement providers, camera offset and validated source zoom procedure",
            "gameplay_verified":false}));
        Ok(component)
    }
}
