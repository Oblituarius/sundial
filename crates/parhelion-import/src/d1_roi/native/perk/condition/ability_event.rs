//! Resolve ability-event operands through stable ability identities in both builds.
use super::*;

/// Source ability-event conditions and the native kind each becomes: the ability starting
/// (kind 10) and ending (kind 11). Both builds give the two the same record, and both native
/// kinds compare the event's first word with the ability reference at +10.
const KINDS: [(u32, u8); 2] = [(0x808031BF, 10), (0x8080308E, 11)];

/// Every Super of one element, by ability identity: Rise of Iron's three and Shadowkeep's,
/// which keeps those identities and adds the newer Supers. A source list naming exactly one
/// element's three is the "when using an Arc-based subclass" test of the elemental armor
/// talents, so the native list names every Shadowkeep Super of that element. Rise of Iron's
/// Arc Blade, Radiance and Ward of Dawn have no Shadowkeep identity. The Shadowkeep sets are
/// each subclass's Super entries, through the equipped row of the entry's ability pool.
const ELEMENTS: [(&str, [u32; 3], &[u32]); 3] = [
    (
        "Arc",
        // Fist of Havoc, Arc Blade, Stormtrance.
        [0xC964_C642, 0x95A5_C27D, 0x59DF_5745],
        // Arc Staff, Fists of Havoc, Thundercrash, Stormtrance, Chaos Reach.
        &[
            0x644B_77ED,
            0xC964_C642,
            0xE081_C63F,
            0x59DF_5745,
            0xED9C_409C,
        ],
    ),
    (
        "Solar",
        // Golden Gun, Radiance, Hammer of Sol.
        [0x3186_FD90, 0xFE43_AC05, 0xBDA3_849C],
        // Golden Gun, Blade Barrage, Hammer of Sol, Burning Maul, Daybreak, Well of Radiance.
        &[
            0x3186_FD90,
            0x3012_8C20,
            0xBDA3_849C,
            0x7F25_A367,
            0x0AAF_5AED,
            0x97BD_F4FA,
        ],
    ),
    (
        "Void",
        // Nova Bomb, Ward of Dawn, Shadowshot.
        [0x073B_5B17, 0x5E4C_7A19, 0x6440_9AED],
        // Shadowshot, Spectral Blades, Sentinel Shield, Nova Bomb, Nova Warp.
        &[
            0x6440_9AED,
            0x3966_2427,
            0x290C_FB64,
            0x073B_5B17,
            0x800A_D2D6,
        ],
    ),
];

pub(super) fn read(
    source: &Source,
    native: &mut Reader,
    p: &Payload,
    at: usize,
) -> Result<NativeNode> {
    let identity = identity(source, p, at)?;
    let target = target(native, identity)?;
    node(p, at, target)
}

/// A complete condition list naming one element's Supers, as one native condition per
/// Shadowkeep Super of that element. Any other list translates node by node.
pub(super) fn element(
    source: &Source,
    native: &mut Reader,
    p: &Payload,
    nodes: &[usize],
) -> Result<Option<Vec<NativeNode>>> {
    let first = *nodes.first().context("Empty source condition list")?;
    let kind = p.u8(first + 5)?;
    if !nodes.iter().all(|&at| {
        p.u32(at - 4)
            .is_ok_and(|class| KINDS.contains(&(class, kind)))
            && p.u8(at + 5).ok() == Some(kind)
    }) {
        return Ok(None);
    }
    let identities = nodes
        .iter()
        .map(|&at| identity(source, p, at))
        .collect::<Result<BTreeSet<_>>>()?;
    let Some((_, _, supers)) = ELEMENTS
        .iter()
        .find(|(_, source, _)| identities == BTreeSet::from(*source))
    else {
        return Ok(None);
    };
    ensure!(
        nodes
            .iter()
            .all(|&at| p.bytes::<5>(at).ok() == p.bytes::<5>(first).ok()),
        "Source element Super conditions have different probabilities"
    );
    supers
        .iter()
        .map(|&identity| node(p, first, target(native, identity)?))
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

/// The Rise of Iron ability identity a qualified ability-event condition names.
fn identity(source: &Source, p: &Payload, at: usize) -> Result<u32> {
    let kind = p.u8(at + 5)?;
    ensure!(
        KINDS.contains(&(p.u32(at - 4)?, kind))
            && (0.0..=1.0).contains(&p.f32(at)?)
            && p.u8(at + 4)? == 255
            && p.u8(at + 6)? == 0
            && p.u32(at + 20)? == 0,
        "Source ability-reference condition has unqualified state or metadata"
    );
    if p.u64(at + 8)? != 0 {
        let path = p.pointer(at + 8)?;
        ensure!(
            p.0[path..].contains(&0),
            "Source ability-reference path is unterminated"
        );
    }
    let pattern = p.u32(at + 16)?;
    ensure!(
        source.reference(pattern)? == 0x80800734,
        "Source ability-reference condition does not select a pattern"
    );
    source.read(pattern)?;
    let assignments = source.runtime_assignments(&mut BTreeMap::new())?;
    let tags = source.classes(0x80803432);
    ensure!(
        tags.len() == 1,
        "Source ability definitions are missing or ambiguous"
    );
    let definitions = source.read(tags[0])?;
    let mut identities = BTreeSet::new();
    let mut matched = None;
    for row in definitions.array(8, 40, Some(0x8080343C))? {
        let identity = definitions.u32(row)?;
        ensure!(
            identities.insert(identity),
            "Duplicate source ability identity"
        );
        if assignments.get(&identity) == Some(&pattern) {
            ensure!(
                matched.replace(identity).is_none(),
                "Source ability pattern has multiple identities"
            );
        }
    }
    matched.context("Source event pattern has no ability identity")
}

/// The Shadowkeep ability pattern an ability identity runs.
fn target(native: &mut Reader, identity: u32) -> Result<u32> {
    let tags = native.classes(0x80805CA4);
    ensure!(
        tags.len() == 1,
        "Native ability definitions are missing or ambiguous"
    );
    let definitions = native.tag(tags[0], Some(0x80805CA4))?;
    let mut identities = BTreeSet::new();
    for row in definitions.array(8, 28, Some(0x80805CAA))? {
        ensure!(
            identities.insert(definitions.u32(row + 4)?),
            "Duplicate native ability identity"
        );
    }
    ensure!(
        identities.contains(&identity),
        "Source event ability {identity:08X} has no native definition"
    );
    let roots = native
        .manager
        .lookup
        .named_tags
        .iter()
        .filter(|tag| tag.name == "investment_assets")
        .map(|tag| tag.hash.0)
        .collect::<BTreeSet<_>>();
    ensure!(
        roots.len() == 1,
        "Native investment assets are missing or ambiguous"
    );
    let root = native.tag(*roots.first().context("Native investment assets")?, None)?;
    let assignments = native.tag(root.u32(16)?, Some(0x80809780))?;
    let mut target = None;
    for row in assignments.array(8, 8, None)? {
        if assignments.u32(row)? == identity {
            ensure!(
                target.replace(assignments.u32(row + 4)?).is_none(),
                "Native event ability has multiple runtime assignments"
            );
        }
    }
    let target = target.context("Native event ability has no runtime assignment")?;
    native.tag(target, Some(0x80809C0F))?;
    Ok(target)
}

fn node(p: &Payload, at: usize, target: u32) -> Result<NativeNode> {
    // Original 76AC40 and native 107C4B0 compare event word zero with the pattern
    // reference at +10. The path is authoring metadata and native stock leaves it null.
    let mut bytes = vec![0; 24];
    bytes[..8].copy_from_slice(&p.bytes::<8>(at)?);
    bytes[16..20].copy_from_slice(&target.to_le_bytes());
    Ok(NativeNode {
        kind: p.u8(at + 5)?,
        bytes,
    })
}
