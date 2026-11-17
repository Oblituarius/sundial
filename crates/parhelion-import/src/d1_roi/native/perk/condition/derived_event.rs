//! A Rise of Iron derived game event that Shadowkeep defines under another name.
//!
//! Both builds' event systems derive events from tables (D1 class `80800659`, two tables;
//! Shadowkeep `80807C9B`, two tables) holding definitions `[hash][0][relptr body]`; a base event's
//! body is D1 `808003BC` / Shadowkeep `8080799D`. A derived
//! event's body (D1 `808003C2`, Shadowkeep `808087F4`) names its parent event and an inline
//! predicate through a relative pointer at `+8`, the predicate's class word just before it. A perk
//! condition only compares the event's name, so a D1 event Shadowkeep lacks is heard through the
//! Shadowkeep event with the same parent chain and the same predicate.
//!
//! The kill-event predicate D1 `808021A3` corresponds to Shadowkeep `80806455`. Its field map was
//! learned from the derived events both builds define (104 of 113 translate exactly; the rest are
//! Bungie's own edits): two leading participant records, the damage declaration at D1 `+40` (the
//! layout [`super::super::filter::damage`] reads: types `+40`/`+50`, include-any labels `+60`,
//! exclude-any labels `+70`) at Shadowkeep `+68` (types `+68`/`+78`, label lists 0 and 2 at
//! `+88`/`+A8`), and the label filter at D1 `+D0` at Shadowkeep `+F8`. A Shadowkeep field D1 has no
//! counterpart for must hold the value every Shadowkeep event D1 also defines holds there, as the
//! range at `+2C0` (−1, −1 on all of them). Equal hashes are not equal events (Shadowkeep's
//! `C06E35EF` differs from D1's), so only this structure decides. `tools/python/d1/d1events.py`
//! runs the same comparison offline.
use super::*;
use std::sync::{Arc, Mutex};

const D1_TABLE: u32 = 0x8080_0659;
const D1_BASE: u32 = 0x8080_03BC;
const D1_DERIVED: u32 = 0x8080_03C2;
const SK_TABLE: u32 = 0x8080_7C9B;
const SK_BASE: u32 = 0x8080_799D;
const SK_DERIVED: u32 = 0x8080_87F4;
const PREDICATE: (u32, u32) = (0x8080_21A3, 0x8080_6455);
/// Shadowkeep's kill-event predicate size.
const SK_PREDICATE_SIZE: usize = 0x2E0;

/// Leading-record bytes and name words, D1 then Shadowkeep predicate offset.
const BYTES: [(usize, usize); 9] = [
    (0x00, 0x00),
    (0x01, 0x01),
    (0x16, 0x2C),
    (0x1C, 0x30),
    (0x1D, 0x31),
    (0x32, 0x5C),
    (0x33, 0x5D),
    (0x34, 0x5E),
    (0x39, 0x61),
];
const WORDS: [(usize, usize); 4] = [(0x04, 0x04), (0x08, 0x08), (0x24, 0x38), (0x2C, 0x58)];
/// Damage declaration types and labels, then the label filter's four lists.
const LISTS: [(usize, usize); 8] = [
    (0x40, 0x68),
    (0x50, 0x78),
    (0x60, 0x88),
    (0x70, 0xA8),
    (0xD0, 0xF8),
    (0xE0, 0x108),
    (0xF0, 0x118),
    (0x100, 0x128),
];

/// A predicate as both builds can compare it: mapped bytes, name words and each list's sorted
/// names (label names, or damage-type bytes).
#[derive(PartialEq, Eq, Hash, Clone, Debug)]
struct Key(Vec<u8>, Vec<u32>, Vec<Vec<u32>>);

struct Table {
    /// Every event row: hash, body offset and body class, from every table of the build.
    rows: BTreeMap<u32, (Payload, usize, u32)>,
    /// The events every table of the build defines.
    everywhere: BTreeSet<u32>,
}

/// Every event definition in `payloads`: each `[hash][0][relptr]` whose target carries a base or
/// derived event body class word. Shadowkeep keeps many derived events nested in other events'
/// bodies rather than in a table's first row array (670 of 3,407 there), so definitions are found
/// by that signature wherever they sit; the exact class word before the target keeps it strict.
fn table(payloads: &[Payload], classes: [u32; 2]) -> Result<Table> {
    let mut rows = BTreeMap::new();
    let mut counts = BTreeMap::<u32, usize>::new();
    for p in payloads {
        let mut seen = BTreeSet::new();
        for at in (0..p.0.len().saturating_sub(16)).step_by(8) {
            if p.u32(at + 4)? != 0 {
                continue;
            }
            let rel = p.u64(at + 8)? as i64;
            if rel <= 0 {
                continue;
            }
            let Some(body) = usize::try_from(rel)
                .ok()
                .and_then(|rel| (at + 8).checked_add(rel))
                .filter(|&body| body >= 4 && body + 16 <= p.0.len())
            else {
                continue;
            };
            let class = p.u32(body - 4)?;
            if !classes.contains(&class) {
                continue;
            }
            let hash = p.u32(at)?;
            if seen.insert(hash) {
                *counts.entry(hash).or_default() += 1;
            }
            // One table of each build keeps every definition with its predicate pointer zeroed
            // (D1 `80A5FFBA`); a definition that names its predicate wins over such a copy.
            let complete = p.u64(body + 8)? != 0;
            match rows.entry(hash) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((p.clone(), body, class));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let (held, at, _) = entry.get();
                    if complete && held.u64(at + 8)? == 0 {
                        entry.insert((p.clone(), body, class));
                    }
                }
            }
        }
    }
    let everywhere = counts
        .into_iter()
        .filter(|&(_, n)| n == payloads.len())
        .map(|(hash, _)| hash)
        .collect();
    Ok(Table { rows, everywhere })
}

fn predicate(p: &Payload, body: usize) -> Result<(u32, usize)> {
    let at = p.pointer(body + 8)?;
    Ok((
        p.u32(at.checked_sub(4).context("Event predicate class")?)?,
        at,
    ))
}

/// A list's names: label rows (24 bytes, name first) or byte enumerations. An offset that holds
/// no array reads as empty.
fn names(p: &Payload, at: usize) -> Vec<u32> {
    let read = || -> Result<Vec<u32>> {
        if p.u64(at)? == 0 {
            return Ok(Vec::new());
        }
        let header = p.pointer(at + 8)?;
        let count = usize::try_from(p.u64(at)?)?;
        ensure!(count < 4096 && p.u64(header)? == count as u64);
        let mut names = match p.u32(header + 8)? {
            0x8080_0365 | 0x8080_94B3 => (0..count)
                .map(|i| p.u32(header + 16 + i * 0x18))
                .collect::<Result<Vec<_>>>()?,
            _ => (0..count)
                .map(|i| p.u8(header + 16 + i).map(u32::from))
                .collect::<Result<Vec<_>>>()?,
        };
        names.sort_unstable();
        Ok(names)
    };
    read().unwrap_or_default()
}

fn key(p: &Payload, at: usize, d1: bool) -> Result<Key> {
    let side = |(d, s): (usize, usize)| if d1 { d } else { s };
    Ok(Key(
        BYTES
            .iter()
            .map(|&pair| p.u8(at + side(pair)))
            .collect::<Result<_>>()?,
        WORDS
            .iter()
            .map(|&pair| p.u32(at + side(pair)))
            .collect::<Result<_>>()?,
        LISTS
            .iter()
            .map(|&pair| names(p, at + side(pair)))
            .collect(),
    ))
}

type Candidates = BTreeMap<(u32, Vec<u8>, Vec<u32>, Vec<Vec<u32>>), Vec<u32>>;

struct Index {
    d1: Table,
    sk: Table,
    /// Shadowkeep kill-event predicates that hold the D1-era values, by parent and key.
    candidates: Candidates,
    /// Every Shadowkeep kill-event predicate by parent and key, whatever its Shadowkeep-only
    /// fields hold: Bungie's revisions of an event.
    revisions: Candidates,
}

impl Index {
    fn build(source: &Source, native: &mut Reader) -> Result<Self> {
        let d1 = source
            .classes(D1_TABLE)
            .into_iter()
            .map(|tag| source.read(tag))
            .collect::<Result<Vec<_>>>()?;
        let sk = native
            .classes(SK_TABLE)
            .into_iter()
            .map(|tag| native.tag(tag, None).map(|p| (*p).clone()))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            !d1.is_empty() && !sk.is_empty(),
            "Event tables are absent from one of the builds"
        );
        let (d1, sk) = (
            table(&d1, [D1_BASE, D1_DERIVED])?,
            table(&sk, [SK_BASE, SK_DERIVED])?,
        );
        // The values every Shadowkeep kill-event predicate holds where its event is also a D1
        // event, at the words the map does not reach.
        let mut mapped = BTreeSet::new();
        for (_, s) in BYTES {
            mapped.insert(s / 4 * 4);
        }
        for (_, s) in WORDS {
            mapped.insert(s);
        }
        for (_, s) in LISTS {
            mapped.extend([s, s + 8]);
        }
        let mut consensus: Option<BTreeMap<usize, u32>> = None;
        for (hash, (p, body, class)) in &sk.rows {
            let Some((dp, dbody, dclass)) = d1.rows.get(hash) else {
                continue;
            };
            if *class != SK_DERIVED || *dclass != D1_DERIVED {
                continue;
            }
            let ((dc, da), (sc, sa)) = (predicate(dp, *dbody)?, predicate(p, *body)?);
            if (dc, sc) != PREDICATE || key(dp, da, true)? != key(p, sa, false)? {
                continue;
            }
            let words = (0..SK_PREDICATE_SIZE)
                .step_by(4)
                .filter(|o| !mapped.contains(o))
                .map(|o| Ok((o, p.u32(sa + o)?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            consensus = Some(match consensus {
                None => words,
                Some(mut kept) => {
                    kept.retain(|o, v| words.get(o) == Some(v));
                    kept
                }
            });
        }
        let consensus = consensus.context("No derived event is defined alike in both builds")?;
        let (mut candidates, mut revisions) = (Candidates::new(), Candidates::new());
        for (&hash, (p, body, class)) in &sk.rows {
            if *class != SK_DERIVED {
                continue;
            }
            let (pc, at) = predicate(p, *body)?;
            if pc != PREDICATE.1 {
                continue;
            }
            let Key(bytes, words, lists) = key(p, at, false)?;
            let entry = (p.u32(*body)?, bytes, words, lists);
            if consensus
                .iter()
                .all(|(&o, &v)| p.u32(at + o).ok() == Some(v))
            {
                candidates.entry(entry.clone()).or_default().push(hash);
            }
            revisions.entry(entry).or_default().push(hash);
        }
        Ok(Self {
            d1,
            sk,
            candidates,
            revisions,
        })
    }

    /// The Shadowkeep event standing for D1 `event`, following its parent chain.
    fn resolve(&self, event: u32, depth: usize) -> Result<Option<u32>> {
        ensure!(depth < 16, "Derived event chain is too deep");
        let Some((p, body, class)) = self.d1.rows.get(&event) else {
            return Ok(None);
        };
        if *class != D1_DERIVED {
            // A base event: both builds name it alike when both define it as a base event.
            return Ok(self
                .sk
                .rows
                .get(&event)
                .is_some_and(|(_, _, c)| *c != SK_DERIVED)
                .then_some(event));
        }
        let Some(parent) = self.resolve(p.u32(*body)?, depth + 1)? else {
            return Ok(None);
        };
        let (class, at) = predicate(p, *body)?;
        if class != PREDICATE.0 {
            return Ok(None);
        }
        // The equal event first. Failing one, the only Shadowkeep event under the same parent
        // whose fields D1 has all hold the source's values: Shadowkeep's revision of the event,
        // which differs only in fields D1 lacks, as Fallen kills `6BC684F5` add an exclusion of
        // Taken (`4F33FBC2`) to D1's `FD485923`.
        let Key(bytes, words, lists) = key(p, at, true)?;
        let entry = (parent, bytes, words, lists);
        Ok(
            match (
                self.candidates.get(&entry).map(Vec::as_slice),
                self.revisions.get(&entry).map(Vec::as_slice),
            ) {
                (Some([one]), _) | (None, Some([one])) => Some(*one),
                _ => None,
            },
        )
    }
}

/// The Shadowkeep event a perk hears in place of D1 `event`, when Shadowkeep lacks `event` and
/// defines a structurally equal derived event in every one of its event tables.
pub(super) fn equivalent(source: &Source, native: &mut Reader, event: u32) -> Result<Option<u32>> {
    static INDEX: Mutex<Option<(std::path::PathBuf, Arc<Index>)>> = Mutex::new(None);
    let place = native.manager.package_dir.clone();
    let cached = crate::cancellation::mutex(&INDEX)?
        .as_ref()
        .filter(|(key, _)| *key == place)
        .map(|(_, index)| index.clone());
    let index = match cached {
        Some(index) => index,
        None => {
            let index = Arc::new(Index::build(source, native)?);
            *crate::cancellation::mutex(&INDEX)? = Some((place, index.clone()));
            index
        }
    };
    Ok(index
        .resolve(event, 0)?
        .filter(|found| index.sk.everywhere.contains(found)))
}
