//! Legacy Frame archetypes. A Rise of Iron legendary weapon's intrinsic names the Shadowkeep frame
//! whose firing its own is nearest, so a player can classify it at a glance.
//!
//! Both games keep firing in the weapon's stat translator: each type's table turns an investment
//! stat into component inputs through sampled curves. Weapon Stats input 0 is Rounds Per Minute
//! (column 0 shots or bursts per second, column 1 bullets per burst) and input 10 a fusion rifle's Charge
//! Time (column 0 seconds). Shown stat numbers are display curves and are never compared: a
//! rocket launcher shown at 25 fires 55 a minute. Fusion charge curves differ between the games
//! (933 to 533 ms against 900 to 500 ms), so investment values do not compare either.
//!
//! The Shadowkeep side is every stock legendary weapon whose intrinsic socket holds a frame plug,
//! with its type, frame, real rate and Impact. It is read once and kept in the importer cache
//! until that file is missing: Sundial's own builds change the package files but not stock frame
//! data, and items from those builds are left out. Manufacturer frames count as their generic
//! frame. The source side replays the weapon's own translator. A type matches the frame with the
//! nearest rate, then the nearest Impact, then the frame more stock weapons of the type use.
//! Swords have no rate or charge row and compare Swing Speed investment values.
use super::*;
use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const FRAME_CATEGORY: u32 = 0x67FB_A961;
const TRANSLATOR: u32 = 0x2F98_1564;
const RATE_OF_FIRE: u32 = 4_284_893_193;
const IMPACT: u32 = 4_043_523_819;
const CHARGE_TIME: u32 = 2_961_396_640;
const SWING_SPEED: u32 = 2_837_207_746;
/// Shadowkeep's package rarity value of a legendary item.
const SHADOWKEEP_LEGENDARY: u8 = 4;
const CACHE_FILE: &str = "shadowkeep-frames-v2.json";
const FUSION: &str = "Fusion Rifle";
const SWORD: &str = "Sword";

/// One stock Shadowkeep legendary weapon's frame.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Frame {
    item: u32,
    kind: String,
    frame: String,
    /// Shots or bullets per minute, or charge milliseconds for a fusion rifle.
    rate: Option<f32>,
    swing: Option<i32>,
    impact: Option<i32>,
}

/// Where a version keeps its stat translator arrays, from the translator definition.
struct Layout {
    groups: (usize, u32),
    keys: (usize, u32),
    translation: u32,
}

const RISE_OF_IRON_PS4: Layout = Layout {
    groups: (0x60, 0x8080_2945),
    keys: (0x228, 0x8080_20A3),
    translation: 0x8080_2A32,
};

const SHADOWKEEP: Layout = Layout {
    groups: (0x48, 0x8080_3975),
    keys: (0x238, 0x8080_38C7),
    translation: 0x8080_3979,
};

/// The outputs of `owner`'s translation row for Weapon Stats `input` in the table of `key`, at
/// investment `value`. A pin the key row sets on that input replaces the value.
fn sample(owner: &Payload, layout: &Layout, key: u32, input: u8, value: f32) -> Result<Vec<f32>> {
    let definition = owner.pointer(24)?;
    let groups = owner.array(definition + layout.groups.0, 48, Some(layout.groups.1))?;
    let keys = owner.array(definition + layout.keys.0, 40, Some(layout.keys.1))?;
    let index = keys
        .iter()
        .position(|&row| owner.u32(row + 0x10).ok() == Some(key))
        .context("Stat translator has no table for the weapon")?;
    let mut stat = value;
    for pin in owner.array(keys[index], 8, None)? {
        if owner.u8(pin)? == input {
            stat = f32::from_bits(owner.u32(pin + 4)?);
        }
    }
    let group = *groups.get(index).context("Stat translator table")?;
    for row in owner.array(group, 56, Some(layout.translation))? {
        if owner.u32(row + 0x20)? != u32::from(input) {
            continue;
        }
        let tiers = owner
            .array(row + 0x10, 16, None)?
            .into_iter()
            .map(|tier| {
                owner
                    .array(tier, 4, None)?
                    .into_iter()
                    .map(|at| Ok(f32::from_bits(owner.u32(at)?)))
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let last = tiers
            .len()
            .checked_sub(1)
            .context("Translation has no tiers")?;
        let offset = i32::from_le_bytes(owner.bytes::<4>(row + 0x24)?);
        let (low, high) = (
            f32::from_bits(owner.u32(row + 0x28)?),
            f32::from_bits(owner.u32(row + 0x2C)?),
        );
        let interpolate = owner.u8(row + 0x30)? != 0;
        let span = high - low;
        let bounded = if span == 0.0 {
            0.0
        } else {
            ((stat - low) * last as f32 / span).clamp(0.0, last as f32)
        };
        let position = bounded + offset as f32;
        let tier = |at: f32| (at as i64 - i64::from(offset)).clamp(0, last as i64) as usize;
        let columns = tiers[0].len();
        return Ok((0..columns)
            .map(|column| {
                if !interpolate {
                    return tiers[tier(position)]
                        .get(column)
                        .copied()
                        .unwrap_or_default();
                }
                let lower = tier(position.floor());
                let from = tiers[lower].get(column).copied().unwrap_or_default();
                let to = tiers[(lower + 1).min(last)]
                    .get(column)
                    .copied()
                    .unwrap_or_default();
                from + (to - from) * (position - position.floor())
            })
            .collect());
    }
    bail!("Stat translator has no row for input {input}")
}

/// The input a type's firing is read from and the stat behind it, if it has one.
fn firing_input(kind: &str) -> Option<(u8, u32)> {
    match kind {
        SWORD => None,
        FUSION => Some((10, CHARGE_TIME)),
        _ => Some((0, RATE_OF_FIRE)),
    }
}

/// Shots per minute, bullets per minute for a burst weapon (column 1 holds more than one bullet
/// per burst, as pulse rifles and burst sidearms do), or a fusion rifle's charge milliseconds.
fn rate(kind: &str, columns: &[f32]) -> Option<f32> {
    let first = *columns.first()?;
    Some(match kind {
        FUSION => first * 1000.0,
        _ => first * 60.0 * columns.get(1).copied().filter(|b| *b > 1.0).unwrap_or(1.0),
    })
}

/// A manufacturer frame's generic frame, such as Hakke Precision Frame's Precision Frame.
fn generic(frame: &str) -> String {
    for (tail, generic) in [
        (" Precision Frame", "Precision Frame"),
        (" Rapid-Fire", "Rapid-Fire Frame"),
        (" Adaptive Frame", "Adaptive Frame"),
    ] {
        if frame.ends_with(tail) && frame != generic {
            return generic.to_owned();
        }
    }
    frame.to_owned()
}

/// Every stock Shadowkeep legendary weapon with a frame in its intrinsic socket.
fn shadowkeep_frames(native: &mut Reader) -> Result<Vec<Frame>> {
    use sundial::package_authoring::{
        entity::weapon_component_bindings,
        investment_schema::{
            GLOBALS_ITEM_STRING_TABLE_SLOT, ITEM_ORDINARY_SOCKET_POINTER_OFFSET,
            ITEM_RARITY_OFFSET, ROOT_ITEM_DEFINITION_TABLE_SLOT, investment_globals_table_tag,
            investment_root_table_tag,
        },
        resolve_item_name, resolve_item_type_name,
        runtime::load_weapon_runtime_entity_for_item_with_manager,
    };
    let manager = crate::tiger::stock::sundial_manager(native)?;
    let globals_tag = native
        .manager
        .lookup
        .named_tags
        .iter()
        .find(|t| t.name == "investment_globals")
        .context("Native investment globals")?
        .hash
        .0;
    let globals = native.tag(globals_tag, None)?;
    let root = native.tag(globals.u32(16)?, Some(0x8080_7D84))?;
    let items = native.tag(
        investment_root_table_tag(&root.0, ROOT_ITEM_DEFINITION_TABLE_SLOT)
            .map_err(anyhow::Error::msg)?,
        None,
    )?;
    let strings = native.tag(
        investment_globals_table_tag(&globals.0, GLOBALS_ITEM_STRING_TABLE_SLOT)
            .map_err(anyhow::Error::msg)?,
        None,
    )?;
    let rows = items.array(8, 24, Some(0x8080_7BE8))?;
    let string_rows = strings.array(8, 24, None)?;
    ensure!(
        rows.len() == string_rows.len(),
        "Native item and string tables differ"
    );
    let definitions = crate::tiger::shadowkeep::stat_definitions(native)?;
    let stat = |hash: u32| definitions.get(&hash).copied();
    let (rpm, impact, charge, swing) = (
        stat(RATE_OF_FIRE).context("Rounds Per Minute definition")?,
        stat(IMPACT).context("Impact definition")?,
        stat(CHARGE_TIME).context("Charge Time definition")?,
        stat(SWING_SPEED).context("Swing Speed definition")?,
    );
    let mut frames = Vec::new();
    for (index, &row) in rows.iter().enumerate() {
        crate::cancellation::check()?;
        let hash = items.u32(row)?;
        let tag = items.u32(row + 16)?;
        // Items Sundial built into the installed packages are not stock.
        let own = native
            .manager
            .package_paths
            .get(&tiger_pkg::TagHash(tag).pkg_id())
            .is_none_or(|package| package.name.starts_with("parhelion"));
        if own || native.reference(tag).ok() != Some(0x8080_7BEA) {
            continue;
        }
        let item = native.tag(tag, Some(0x8080_7BEA))?;
        if item.u8(ITEM_RARITY_OFFSET)? != SHADOWKEEP_LEGENDARY
            || item.u64(ITEM_ORDINARY_SOCKET_POINTER_OFFSET)? == 0
        {
            continue;
        }
        let sockets = item.array(
            item.pointer(ITEM_ORDINARY_SOCKET_POINTER_OFFSET)?,
            0x50,
            Some(0x8080_77C4),
        )?;
        let Some(&intrinsic) = sockets.first() else {
            continue;
        };
        let default = item.u16(intrinsic + 2)?;
        let Some(&plug_row) = rows.get(usize::from(default)) else {
            continue;
        };
        let plug = native.tag(items.u32(plug_row + 16)?, None)?;
        if crate::tiger::shadowkeep::plug_category(&plug)? != Some(FRAME_CATEGORY) {
            continue;
        }
        let name_of = |row: usize| -> Option<String> {
            let tag = strings.u32(row + 16).ok()?;
            resolve_item_name(&manager, tiger_pkg::TagHash(tag)).ok()
        };
        // Exotic intrinsics share the category. Frames name themselves.
        let Some(frame) = name_of(string_rows[usize::from(default)]).filter(|n| {
            ["Frame", "Burst", "Rapid-Fire"]
                .iter()
                .any(|w| n.contains(w))
        }) else {
            continue;
        };
        let Some(kind) = strings
            .u32(string_rows[index] + 16)
            .ok()
            .and_then(|tag| resolve_item_type_name(&manager, tiger_pkg::TagHash(tag)).ok())
        else {
            continue;
        };
        let values = crate::tiger::shadowkeep::investment_stats(native, hash)?;
        let value = |index: u16| values.get(&index).copied();
        let rate = match firing_input(&kind) {
            Some((input, _)) => {
                let investment = value(if input == 10 { charge } else { rpm });
                let runtime = load_weapon_runtime_entity_for_item_with_manager(&manager, hash);
                match (investment, runtime) {
                    (Some(investment), Ok(runtime)) => {
                        let bindings = weapon_component_bindings(&runtime.payload, TRANSLATOR)
                            .map_err(anyhow::Error::msg)?;
                        let [binding] = bindings.as_slice() else {
                            continue;
                        };
                        let owner = native.tag(binding.owner_tag, Some(0x8080_9C36))?;
                        sample(
                            &owner,
                            &SHADOWKEEP,
                            runtime.weapon_translation_group_hash,
                            input,
                            investment as f32,
                        )
                        .ok()
                        .and_then(|columns| self::rate(&kind, &columns))
                    }
                    _ => None,
                }
            }
            None => None,
        };
        frames.push(Frame {
            item: hash,
            kind,
            frame: generic(&frame),
            rate,
            swing: value(swing),
            impact: value(impact),
        });
    }
    ensure!(!frames.is_empty(), "No stock legendary weapon has a frame");
    Ok(frames)
}

/// The stock frame table, read once and then kept until its cache file is missing.
fn frames(native: &mut Reader) -> Result<Vec<Frame>> {
    let path =
        crate::cache::importer_cache("d1_roi", "legacy-frames").map(|root| root.join(CACHE_FILE));
    if let Some(frames) = path
        .as_ref()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<Frame>>(&bytes).ok())
    {
        return Ok(frames);
    }
    let frames = shadowkeep_frames(native)?;
    if let Some(path) = path {
        let _ = crate::cache::write_json(&path, &frames);
    }
    Ok(frames)
}

/// The source weapon's own firing: its stat translator's row at its investment value.
fn source_rate(source: &Source, export: &Value, kind: &str) -> Result<Option<f32>> {
    let Some((input, stat)) = firing_input(kind) else {
        return Ok(None);
    };
    let value = export["stats"]["rows"]
        .as_array()
        .context("Source base stats")?
        .iter()
        .find(|row| row["hash"].as_u64() == Some(u64::from(stat)))
        .and_then(|row| row["value"].as_f64())
        .context("Source weapon has no firing stat")?;
    let (_, patterns) = source.table(0x8080_339A)?;
    let index = usize::try_from(
        export["runtime"]["pattern_index"]
            .as_u64()
            .context("Source pattern index")?,
    )?;
    let pattern = *patterns
        .array(8, 32, Some(0x8080_33BC))?
        .get(index)
        .context("Source firing pattern")?;
    let key = patterns.u32(pattern + 8)?;
    let root = u32::from_str_radix(
        export["runtime"]["runtime_entity"]
            .as_str()
            .context("Source firing runtime")?,
        16,
    )?;
    let entity = source.read(root)?;
    let mut owners = Vec::new();
    for row in entity.array(32, 12, Some(0x8080_0715))? {
        let owner = source.read(entity.u32(row)?)?;
        let definition = owner.pointer(24)?;
        if owner
            .array(
                definition + RISE_OF_IRON_PS4.groups.0,
                48,
                Some(RISE_OF_IRON_PS4.groups.1),
            )
            .is_ok_and(|groups| !groups.is_empty())
        {
            owners.push(owner);
        }
    }
    let [owner] = owners.as_slice() else {
        bail!("Source firing translator is missing or ambiguous");
    };
    let columns = sample(owner, &RISE_OF_IRON_PS4, key, input, value as f32)?;
    Ok(rate(kind, &columns))
}

/// The archetype line of a PS4 legendary weapon's Legacy Frame, such as "Precision Frame,
/// 450 RPM." or "High-Impact Frame, 843 ms Charge Time.", or `None` when no stock frame of its
/// type compares.
pub(in crate::d1_roi::native) fn archetype(
    source: &Source,
    native: &mut Reader,
    export: &Value,
) -> Result<Option<String>> {
    let kind = export["item"]["kind"]
        .as_str()
        .context("Source weapon type")?
        .to_owned();
    let stat = |hash: u32| {
        export["stats"]["rows"].as_array().and_then(|rows| {
            rows.iter()
                .find(|row| row["hash"].as_u64() == Some(u64::from(hash)))
                .and_then(|row| row["value"].as_i64())
        })
    };
    let impact = stat(IMPACT);
    let (measure, rate) = if kind == SWORD {
        (stat(SWING_SPEED).map(|v| v as f32), None)
    } else {
        let rate = source_rate(source, export, &kind)?;
        (rate, rate)
    };
    let Some(measure) = measure else {
        return Ok(None);
    };
    // Frames of the type by (frame, measure): the measure, each holder's Impact and the holders.
    let mut options = BTreeMap::<(String, i64), (f32, Vec<i32>, usize)>::new();
    for frame in frames(native)?.into_iter().filter(|f| f.kind == kind) {
        let value = if kind == SWORD {
            frame.swing.map(|v| v as f32)
        } else {
            frame.rate
        };
        let Some(value) = value else {
            continue;
        };
        let entry = options
            .entry((frame.frame, (value * 10.0).round() as i64))
            .or_insert((value, Vec::new(), 0));
        entry.1.extend(frame.impact);
        entry.2 += 1;
    }
    let impact = impact.map_or(0.0, |v| v as f32);
    let best = options
        .into_iter()
        .min_by(|(_, (a, ai, an)), (_, (b, bi, bn))| {
            let nearest = |v: f32, impacts: &[i32]| {
                (
                    (v - measure).abs(),
                    impacts
                        .iter()
                        .map(|i| (*i as f32 - impact).abs())
                        .fold(f32::INFINITY, f32::min),
                )
            };
            let (da, ia) = nearest(*a, ai);
            let (db, ib) = nearest(*b, bi);
            da.total_cmp(&db).then(ia.total_cmp(&ib)).then(bn.cmp(an))
        });
    let Some(((frame, _), _)) = best else {
        return Ok(None);
    };
    Ok(Some(match (kind.as_str(), rate) {
        (FUSION, Some(ms)) => format!("{frame}, {ms:.0} ms Charge Time."),
        (_, Some(rpm)) => format!("{frame}, {rpm:.0} RPM."),
        _ => format!("{frame}."),
    }))
}
