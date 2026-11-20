//! Rise of Iron sight columns on Shadowkeep's sight socket.
//!
//! Rise of Iron keeps no column category in its talent grids, but every sight and scope step
//! carries the sight index stat, which says which sight model the weapon shows. A column that
//! offers a choice of two or more steps, most of them carrying it, is a sight column. It takes
//! the sight socket type, so Parhelion and the inspect screen show it as the weapon type's Scope
//! or Sight row instead of the donor's Barrel, and its plugs take a stock sight plug's layout.
//! The plugs keep their source stats, so the sight index, and with it the model, stays the
//! source's: the lent plug has no art of its own.
use super::*;

/// Shadowkeep's sight socket type, which weapon types label Scope or Sight.
pub(super) const SIGHT_SOCKET_TYPE: u16 = 179;

/// The unnamed investment stat both games' sight plugs carry, Shadowkeep's definition 33. Its
/// value indexes the sight parts of the weapon's art (selectors 3 and 21): FastDraw IS 0,
/// SteadyHand IS 1 … Focus Lens FLA5 13, the same on every weapon type. Among every Rise of Iron
/// weapon's steps (tools `d1/d1columns.py marker`, 2026-11-18), all 67 sight and scope steps
/// carry it, even at zero as Khvostov 7G-0X's Classic Scope does, and no other step or
/// single-step column does. A perk that mentions a sight, such as Third Eye or Custom Optics,
/// does not carry it.
const SIGHT_INDEX: u64 = 0xC430_48A0;

fn carries_index(choice: &Choice) -> bool {
    choice
        .lowered
        .adjustments
        .iter()
        .any(|row| row["source_hash"].as_u64() == Some(SIGHT_INDEX))
}

/// Whether a column offers two or more steps, most of which carry the sight index.
pub(super) fn column(choices: &[Choice], members: &[usize]) -> bool {
    let sights = members
        .iter()
        .filter(|&&index| carries_index(&choices[index]))
        .count();
    members.len() >= 2 && 2 * sights > members.len()
}

/// The sight socket type and a stock sight plug of the donor's weapon type, the default plug of
/// the first stock weapon of that type with a sight socket. None when no such weapon exists, as
/// for types that carry no sight.
pub(super) fn layout(native: &mut Reader, donor: u32) -> Result<Option<(u16, u32)>> {
    use sundial::package_authoring::{
        investment_schema::{
            GLOBALS_ITEM_STRING_TABLE_SLOT, ITEM_ORDINARY_SOCKET_POINTER_OFFSET,
            ROOT_ITEM_DEFINITION_TABLE_SLOT, investment_globals_table_tag,
            investment_root_table_tag,
        },
        resolve_item_type_name,
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
    let type_name = |index: usize| -> Option<String> {
        let tag = strings.u32(string_rows[index] + 16).ok()?;
        resolve_item_type_name(&manager, tiger_pkg::TagHash(tag)).ok()
    };
    let donor_index = rows
        .iter()
        .position(|&row| items.u32(row).ok() == Some(donor))
        .context("Native donor item row")?;
    let Some(kind) = type_name(donor_index) else {
        return Ok(None);
    };
    // The search reads many definitions, so it keeps its reads apart.
    let mut scan = native.shared_discovery();
    for (index, &row) in rows.iter().enumerate() {
        crate::cancellation::check()?;
        let tag = items.u32(row + 16)?;
        // Items Sundial built into the installed packages are not stock.
        let own = scan
            .manager
            .package_paths
            .get(&tiger_pkg::TagHash(tag).pkg_id())
            .is_none_or(|package| package.name.starts_with("parhelion"));
        if own || scan.reference(tag).ok() != Some(0x8080_7BEA) {
            continue;
        }
        let item = scan.tag(tag, Some(0x8080_7BEA))?;
        if item.u64(ITEM_ORDINARY_SOCKET_POINTER_OFFSET)? == 0 {
            continue;
        }
        let sockets = item.array(
            item.pointer(ITEM_ORDINARY_SOCKET_POINTER_OFFSET)?,
            0x50,
            Some(0x8080_77C4),
        )?;
        let Some(default) = sockets.iter().find_map(|&socket| {
            (item.u16(socket).ok()? == SIGHT_SOCKET_TYPE)
                .then(|| item.u16(socket + 2).ok())
                .flatten()
                .filter(|&default| default != u16::MAX)
        }) else {
            continue;
        };
        if type_name(index).as_deref() != Some(kind.as_str()) {
            continue;
        }
        let plug = *rows
            .get(usize::from(default))
            .context("Native sight plug row")?;
        return Ok(Some((SIGHT_SOCKET_TYPE, items.u32(plug)?)));
    }
    Ok(None)
}
