//! Donor sockets a Rise of Iron weapon has no counterpart for.
//!
//! The import authors the intrinsic, perk, shader and damage-mod lanes, and the ornament lane when
//! source ornaments bind. Every other donor lane would offer the donor's own plugs, such as an
//! exotic's catalyst, a legendary's masterwork, or stock ornaments when none of the source's bind,
//! none of which existed in Rise of Iron. Those lanes are disabled the way a native unused socket
//! row is, as the armor import does past its finished layout.
use super::*;

/// The donor lanes that are already unused socket rows, from its native socket report.
pub(super) fn unused_lanes(native: &Value) -> Result<BTreeSet<usize>> {
    Ok(native["sockets"]
        .as_array()
        .context("Native sockets")?
        .iter()
        .enumerate()
        .filter(|(_, socket)| socket["socket_type"] == u16::MAX)
        .map(|(lane, _)| lane)
        .collect())
}

/// Disable every lane of the recipe at `recipe_path` that no translation authored, after the
/// ornaments bind. Returns the report of the lanes it disabled.
///
/// A lane in `unused` is one the donor already leaves as an unused row. It stays untouched: the
/// build rejects a socket override on a row with nothing to author, and the row is already what
/// a disabled lane becomes.
pub(super) fn disable_untouched(recipe_path: &Path, unused: &BTreeSet<usize>) -> Result<Value> {
    let mut recipe: Value = serde_json::from_slice(&fs::read(recipe_path)?)?;
    let columns = recipe["overrides"]["socket_columns"]
        .as_array_mut()
        .context("Weapon socket columns")?;
    let mut disabled = Vec::new();
    for (lane, column) in columns.iter_mut().enumerate() {
        if column.is_null() && !unused.contains(&lane) {
            *column = json!({"choices":[],"socket_type":u16::MAX});
            disabled.push(lane);
        }
    }
    if !disabled.is_empty() {
        crate::io::write_json(recipe_path, &recipe)?;
    }
    let reason = "Rise of Iron weapons have no catalyst, masterwork or stock ornament sockets, so donor lanes no translation authored are disabled.";
    Ok(json!({"disabled_donor_sockets":disabled,"unused_donor_sockets":unused,"reason":reason}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_donor_lanes_keep_no_override() -> Result<()> {
        let native = json!({"sockets":[
            {"socket_type":176},{"socket_type":u16::MAX},{"socket_type":188}]});
        let unused = unused_lanes(&native)?;
        assert_eq!(unused, BTreeSet::from([1]));
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("weapon.parhelion.json");
        let authored = json!({"choices":["0x00000001"],"socket_type":176});
        crate::io::write_json(
            &path,
            &json!({"overrides":{"socket_columns":[authored, null, null]}}),
        )?;
        let report = disable_untouched(&path, &unused)?;
        let recipe: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let columns = &recipe["overrides"]["socket_columns"];
        assert_eq!(columns[0], authored);
        assert!(columns[1].is_null());
        assert_eq!(columns[2], json!({"choices":[],"socket_type":u16::MAX}));
        assert_eq!(report["disabled_donor_sockets"], json!([2]));
        assert_eq!(report["unused_donor_sockets"], json!([1]));
        Ok(())
    }
}
