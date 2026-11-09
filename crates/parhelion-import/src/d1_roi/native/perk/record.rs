//! Typed host records, such as the record that holds a player in the air while aiming.
use super::*;
use sundial::package_authoring::sandbox_perk::action::native::{Block, Graph, NodeKind};

/// Source effect class `808023AA` (kind 31) and native class `80803E19` (kind 36) both register
/// the record their `+8` pointer names on the host and remove it on removal.
const SOURCE_RECORD: u32 = 0x8080_2AA6;
const NATIVE_RECORD: u32 = 0x8080_692C;
/// The aim-suspension record: Empyrean Bellicose's Antigrav Thrusters in Rise of Iron and Wings of
/// Sacred Dawn's Tome of Dawn (perk 196) in Shadowkeep. The native record holds the source
/// record's ten values in the same order after a flags word. Tome of Dawn's are 10000, -1,
/// 0.125, 0.8, 0.8, 0.15, -1, -0.1, 0, -1, equal to Antigrav Thrusters' except the first, which
/// Antigrav sets to its 2 s hold and Tome of Dawn leaves unbounded, ending the hold by timers.
const SUSPENSION: u32 = 0xA53C_CCDA;
/// Tome of Dawn's flags word, the only stock value for this record type.
const SUSPENSION_FLAGS: u32 = 0x0000_0101;
const VALUES: usize = 10;
/// Both records are 72 bytes: the type, then the values. The native flags word takes one of the
/// source's trailing zero words.
const RECORD_SIZE: usize = 72;

pub(super) fn read(p: &Payload, at: usize) -> Result<Action> {
    ensure!(
        p.bytes::<7>(at + 1)? == [0; 7],
        "Source host record effect has unknown header fields"
    );
    let record = p.pointer(at + 8)?;
    ensure!(
        p.u32(record - 4)? == SOURCE_RECORD && p.u32(record)? == SUSPENSION,
        "Source host record type has no native contract"
    );
    let values = (0..VALUES)
        .map(|index| p.f32(record + 4 + index * 4))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        values.iter().all(|value| value.is_finite())
            && p.0.get(record + 4 + VALUES * 4..record + RECORD_SIZE) == Some(&[0; 28][..]),
        "Source host record has nonfinite values or trailing data"
    );
    Ok(Action::Native {
        node: node(&values)?,
    })
}

fn node(values: &[f32]) -> Result<NativeNode> {
    let mut record = vec![0; RECORD_SIZE];
    put(&mut record, 0, &SUSPENSION.to_le_bytes())?;
    put(&mut record, 4, &SUSPENSION_FLAGS.to_le_bytes())?;
    for (index, value) in values.iter().enumerate() {
        put(&mut record, 8 + index * 4, &value.to_le_bytes())?;
    }
    let mut bytes = vec![0; 16];
    bytes[0] = 36;
    let graph = Graph {
        blocks: vec![
            Block {
                class: 0x8080_3E19,
                count: None,
                bytes,
                links: BTreeMap::from([(8, 1)]),
            },
            Block {
                class: NATIVE_RECORD,
                count: None,
                bytes: record,
                links: BTreeMap::new(),
            },
        ],
    };
    graph
        .validate_node(NodeKind::Effect(36))
        .map_err(anyhow::Error::msg)?;
    Ok(NativeNode {
        kind: 36,
        bytes: graph.emit().map_err(anyhow::Error::msg)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tome of Dawn's own values rebuild its stock node byte for byte (perk 196, `sunprobe perk
    /// 196 --raw`).
    #[test]
    fn suspension_record_matches_tome_of_dawn() {
        let stock = hex::decode(
            "240000000000000018000000000000000000000000000000000000002c698080dacc3ca50101000000401c46000080bf0000003ecdcc4c3fcdcc4c3f9a99193e000080bfcdccccbd00000000000080bf000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let node = node(&[10000.0, -1.0, 0.125, 0.8, 0.8, 0.15, -1.0, -0.1, 0.0, -1.0]).unwrap();
        assert_eq!(node.bytes, stock);
    }
}
