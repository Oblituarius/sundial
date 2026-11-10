//! Donor sockets a Rise of Iron weapon has no counterpart for.
//!
//! The import authors the intrinsic, perk, shader and damage-mod lanes, and the ornament lane when
//! source ornaments bind. Every other donor lane would offer the donor's own plugs, such as an
//! exotic's catalyst, a legendary's masterwork, or stock ornaments when none of the source's bind,
//! none of which existed in Rise of Iron. Those lanes are disabled the way a native unused socket
//! row is, as the armor import does past its finished layout.
use super::*;

/// Disable every lane of the recipe at `recipe_path` that no translation authored, after the
/// ornaments bind. Returns the report of the lanes it disabled.
pub(super) fn disable_untouched(recipe_path: &Path) -> Result<Value> {
    let mut recipe: Value = serde_json::from_slice(&fs::read(recipe_path)?)?;
    let columns = recipe["overrides"]["socket_columns"]
        .as_array_mut()
        .context("Weapon socket columns")?;
    let mut disabled = Vec::new();
    for (lane, column) in columns.iter_mut().enumerate() {
        if column.is_null() {
            *column = json!({"choices":[],"socket_type":u16::MAX});
            disabled.push(lane);
        }
    }
    if !disabled.is_empty() {
        crate::io::write_json(recipe_path, &recipe)?;
    }
    Ok(json!({"disabled_donor_sockets":disabled,
        "reason":"Rise of Iron weapons have no catalyst, masterwork or stock ornament sockets, so donor lanes no translation authored are disabled."}))
}
