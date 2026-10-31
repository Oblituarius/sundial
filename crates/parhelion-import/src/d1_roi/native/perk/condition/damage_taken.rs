//! Taking damage that matches a damage-type and damage-source filter.
use super::*;
use sundial::package_authoring::sandbox_perk::action::native::{Block, Graph, NodeKind};

/// Source kind 5 (8080313D) carries the damage filter hit and kill events use at +8, with its
/// label dictionary at +D8. Native kind 5 (80803DDC) holds the same filter at +8: its inline
/// ability filter sits at +78, where stock Taken Barrier, Fallen Barrier and the third stock
/// owner keep their two "any" flags at +90. Twilight Garrison's Recovery talents filter Arc,
/// Solar or Void damage with the splash label.
pub(super) fn read(
    source: &Source,
    p: &Payload,
    at: usize,
    native: Option<&mut Reader>,
) -> Result<NativeNode> {
    ensure!(
        (0.0..=1.0).contains(&p.f32(at)?)
            && p.u8(at + 4)? == 0xFF
            && p.u8(at + 6)? == 0
            && p.bytes::<0x44>(at + 0x94)? == [0; 0x44],
        "Source damage-taken condition has a threshold, key, distance or state"
    );
    let mut bytes = vec![0; 0xC8];
    bytes[..8].copy_from_slice(&p.bytes::<8>(at)?);
    // Every stock node leaves the named key empty and the distance unlimited.
    put(&mut bytes, 0xA0, &0x811C9DC5u32.to_le_bytes())?;
    put(&mut bytes, 0xA4, &(-1.0f32).to_le_bytes())?;
    let mut graph = Graph {
        blocks: vec![Block {
            class: 0x80803DDC,
            count: None,
            bytes,
            links: BTreeMap::new(),
        }],
    };
    filter::damage(source, p, at + 8, at + 0xD8, &mut graph, 8, native)?;
    graph
        .validate_node(NodeKind::Condition(5))
        .map_err(anyhow::Error::msg)?;
    Ok(NativeNode {
        kind: 5,
        bytes: graph.emit().map_err(anyhow::Error::msg)?,
    })
}
