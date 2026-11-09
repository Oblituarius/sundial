//! First-person animations a converted Rise of Iron weapon takes from a stock weapon.
//!
//! A converted weapon plays its base's first-person animations. Shadowkeep keeps the
//! attachment rows of some Rise of Iron content groups that no stock weapon selects any more
//! (The First Curse's `E5D04AD2` sits beside The Last Word's `DFD28F30`), so what the source
//! played can still be read action by action from the shared arms rig. The conversion lends
//! those actions from the first base candidate that plays them, the whole profile when one does.
use super::*;
use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Writes the animation donor or per-action donors of the converted weapon at `recipe` into it.
/// `candidates` are the stock weapons of its type in the order bases are tried.
pub(super) fn lend(packages: &Path, recipe: &Path, candidates: &[u32]) -> Result<()> {
    let mut prepared: Value = serde_json::from_slice(&std::fs::read(recipe)?)?;
    let Some(directory) = prepared["overrides"]["imported_graph"]["directory"].as_str() else {
        return Ok(());
    };
    let graph: Value = serde_json::from_slice(&std::fs::read(
        Path::new(directory).join("asset-graph.json"),
    )?)?;
    let Some(group) = graph["source_content_group"].as_u64() else {
        return Ok(());
    };
    let group = u32::try_from(group).context("Source content group")?;
    let base = prepared["donor"]["item_hash"]
        .as_str()
        .and_then(|hash| u32::from_str_radix(hash.trim_start_matches("0x"), 16).ok())
        .context("Converted weapon base")?;
    let manager = sundial::package_authoring::open_shadowkeep_package_manager(packages)
        .map_err(|error| anyhow::anyhow!(error))?;
    let candidates = candidates
        .iter()
        .copied()
        .filter(|item| *item != base)
        .collect::<Vec<_>>();
    let Some(lent) = crate::weapon::lenders::group_animations(&manager, base, group, &candidates)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
    else {
        return Ok(());
    };
    let reference = |item: u32| json!({"item_hash":format!("0x{item:08X}")});
    let overrides = &mut prepared["overrides"];
    if let Some(donor) = lent.donor {
        overrides["animation_donor"] = reference(donor);
    }
    if !lent.actions.is_empty() {
        overrides["animation_actions"] = lent
            .actions
            .iter()
            .map(|(action, item)| {
                Ok((
                    serde_json::to_value(action)?
                        .as_str()
                        .context("Action")?
                        .to_owned(),
                    reference(*item),
                ))
            })
            .collect::<Result<serde_json::Map<_, _>>>()?
            .into();
    }
    parhelion_import::io::write_json(recipe, &prepared)?;
    Ok(())
}

/// A reimport takes the conversion's animation choices only when the saved entry made none, so
/// a donor chosen in the workbench stays.
pub(super) fn merge(prepared: &WeaponRecipe, updated: &mut WeaponRecipe) {
    if updated.overrides.animation_donor.is_none() && updated.overrides.animation_actions.is_empty()
    {
        updated.overrides.animation_donor = prepared.overrides.animation_donor.clone();
        updated.overrides.animation_actions = prepared.overrides.animation_actions.clone();
    }
}
