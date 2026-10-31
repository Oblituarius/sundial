//! A modifier bank that shares its pattern with a status host and that status's presentation.
//!
//! Twilight Garrison's elemental talents attach one pattern holding a modifier bank, a status
//! host, a scalar sequence the status state drives, the channel bank that sequence feeds back
//! into the status, and a network replication component. Nothing outside the status, its
//! presentation and the replicator is connected, so the bank is the whole gameplay effect.
//! Armor's status has an empty HUD name and no time limit, so it shows nothing and only the
//! bank is lowered. Recovery's names a HUD buff, which the status lowering keeps.
use super::*;

pub(super) struct Presented {
    pub bank: u32,
    pub rows: Vec<Modifier>,
    /// The status host when it shows the player a HUD buff.
    pub status: Option<(u32, Payload)>,
    /// Components the lowering leaves out: the presentation, and a HUD-less status.
    pub omitted: usize,
}

pub(super) fn bank(source: &Source, graph: &Payload) -> Result<Option<Presented>> {
    let owners = graph
        .array(32, 12, Some(0x80800715))?
        .into_iter()
        .map(|at| graph.u32(at))
        .collect::<Result<Vec<_>>>()?;
    let (mut bank, mut status) = (None, None);
    let mut presentation = BTreeSet::new();
    for &owner in &owners {
        if source.reference(owner)? != 0x80800861 {
            return Ok(None);
        }
        let p = source.read(owner)?;
        let runtime = p.u32(p.pointer(16)? - 4)?;
        let settings = p.u32(p.pointer(24)? - 4)?;
        match (runtime, settings) {
            (0x808021F1, 0x808020DE) if bank.is_none() => bank = Some(owner),
            (0x80802A75, 0x80802698) if status.is_none() => status = Some((owner, p)),
            // A scalar sequence and a channel bank only animate the status's presentation.
            (0x8080079A, 0x80800610) | (0x80800594, 0x808008F6) => {
                presentation.insert(owner);
            }
            _ if replicator(source, runtime)? => {
                presentation.insert(owner);
            }
            _ => return Ok(None),
        }
    }
    let (Some(bank), Some((status, p))) = (bank, status) else {
        return Ok(None);
    };
    if presentation.is_empty() {
        return Ok(None);
    }
    let settings = p.pointer(24)?;
    let hud = settings + source.field(0xF0, 0xD0);
    let duration = hud + source.field(0x160, 0x140);
    let seconds = duration + source.field(0x138, 0xF0);
    for (at, class) in [
        (hud, 0x8080278D),
        (duration, 0x8080255D),
        (seconds, 0x808008AC),
    ] {
        ensure!(
            p.u32(at)? == status && p.u32(at + 16)? == class,
            "Source presented status has a different layout"
        );
    }
    let shown = p.u32(hud + source.field(0x88, 0x68))? != 0x811C9DC5;
    // A status that shows nothing may only stand for the whole attachment's lifetime.
    ensure!(
        shown || f32::from_le_bytes(status::constant(&p, seconds, true)?[..4].try_into()?) == -1.0,
        "Source HUD-less status has its own duration"
    );
    // Every connection stays among the status and its presentation, so the bank has no driver.
    let omitted = presentation.len() + usize::from(!shown);
    presentation.insert(status);
    ensure!(
        graph.bytes::<16>(0x40)? == [0; 16],
        "Source presented status has additional event connections"
    );
    for at in graph.array(0x30, 88, Some(0x8080054E))? {
        ensure!(
            presentation.contains(&graph.u32(at + 8)?)
                && presentation.contains(&graph.u32(at + 48)?),
            "Source presented status connects outside its presentation"
        );
    }
    let indices = graph.array(0x88, 2, Some(0x80800006))?;
    ensure!(
        indices.iter().all(|&a| graph.u16(a).ok() == Some(0)),
        "Source presented status owner indices differ"
    );
    let rows = modifier_owner(source, graph, bank)?;
    Ok(Some(Presented {
        bank,
        rows,
        status: shown.then_some((status, p)),
        omitted,
    }))
}

/// Whether a component class is the engine's network replication component, which a native
/// attachment brings with it. Its schema tag names the class, the same in both builds.
fn replicator(source: &Source, class: u32) -> Result<bool> {
    if source.reference(class).ok() != Some(0x80800000) {
        return Ok(false);
    }
    let schema = source.read(class)?;
    let name = b"c_network_component_tag_instance";
    Ok(schema.0.windows(name.len()).any(|window| window == name))
}
