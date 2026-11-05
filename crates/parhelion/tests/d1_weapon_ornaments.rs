//! A prepared D1 weapon recipe's ornaments staged into native packages and read back.
#![cfg(feature = "d2-model-importer")]
use parhelion::{
    BatchBuildRequest, BatchBuildSnapshot, WeaponRecipe, build_and_stage_snapshot_with_progress,
};
use parhelion_import::tiger::{
    payload::Payload,
    reader::{Reader, package_stamp},
};
use serde_json::Value;
use std::{collections::BTreeSet, env, fs, path::PathBuf};

/// Every key a weapon art row's slots assign.
fn row_keys(art: &Payload, row: usize) -> Result<BTreeSet<u32>, Box<dyn std::error::Error>> {
    let mut keys = BTreeSet::new();
    if art.u64(row + 16)? == 0 {
        return Ok(keys);
    }
    for entry in art.array(row + 16, 8, None)? {
        let resource = art.pointer(entry)?;
        for at in art.array(resource + 8, 4, None)? {
            keys.insert(art.u32(at)?);
        }
    }
    keys.retain(|key| ![0, u32::MAX, 0x811C9DC5].contains(key));
    Ok(keys)
}

fn plug_keys(
    readback: &mut Reader,
    definition: u32,
    art: &Payload,
    art_rows: &[usize],
) -> Result<BTreeSet<u32>, Box<dyn std::error::Error>> {
    let plug = readback.tag(definition, Some(0x80807BEA))?;
    let mut keys = BTreeSet::new();
    for selection in plug.array(plug.pointer(0x88)?, 4, Some(0x808077B5))? {
        let row = *art_rows
            .get(usize::from(plug.u16(selection + 2)?))
            .ok_or("Unregistered ornament art")?;
        keys.extend(row_keys(art, row)?);
    }
    Ok(keys)
}

#[test]
#[ignore = "Requires SUNDIAL_STOCK_PACKAGES, a prepared PARHELION_D1_WEAPON_RECIPE and a fresh PARHELION_D1_NATIVE_OUTPUT"]
#[allow(clippy::cognitive_complexity)]
fn source_weapon_ornaments_stage_as_private_socket_choices()
-> Result<(), Box<dyn std::error::Error>> {
    let configured = |key| {
        env::var_os(key)
            .map(PathBuf::from)
            .ok_or_else(|| format!("Set {key}"))
    };
    let native = configured("SUNDIAL_STOCK_PACKAGES")?;
    let output = parhelion_import::tiger::reader::outside(
        &configured("PARHELION_D1_NATIVE_OUTPUT")?,
        &native,
    )?;
    assert!(!output.exists(), "Use a fresh native artifact directory");
    fs::create_dir_all(&output)?;
    let recipe = WeaponRecipe::load_json(configured("PARHELION_D1_WEAPON_RECIPE")?)?;
    let reference = recipe
        .overrides
        .imported_graph
        .as_ref()
        .ok_or("Imported weapon art missing")?;
    assert!(
        !reference.attachments.is_empty(),
        "The recipe has no ornament attachments"
    );
    let links = reference
        .attachments
        .iter()
        .map(|attachment| {
            let graph: Value =
                serde_json::from_slice(&fs::read(attachment.directory.join("asset-graph.json"))?)?;
            Ok::<_, Box<dyn std::error::Error>>(graph["ornament"].clone())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let socket = links[0]["target_socket"]
        .as_u64()
        .ok_or("Ornament socket")? as usize;
    let reset = links[0]["default_reset"].as_u64().ok_or("Reset")? as u32;
    let stamp = package_stamp(&native)?;
    let snapshot = BatchBuildSnapshot::new(BatchBuildRequest {
        package_directory: native.clone(),
        staging_root: output.join("build"),
        ignore_installed_authored_overlays: true,
        maps: Vec::new(),
        recipes: vec![recipe.clone()],
    })?;
    let built =
        build_and_stage_snapshot_with_progress(&snapshot, |step| println!("{:?}", step.phase))?;
    assert_eq!(
        stamp,
        package_stamp(&native)?,
        "Staging changed stock packages"
    );
    // Read the staged packages over the stock ones in a fresh view of hard links.
    let packages = output.join("view/packages");
    fs::create_dir_all(&packages)?;
    for entry in fs::read_dir(&native)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|s| s == "pkg") {
            fs::hard_link(entry.path(), packages.join(entry.file_name())).map_err(|error| {
                format!(
                    "Link stock package {}: {error}. PARHELION_D1_NATIVE_OUTPUT must be on the stock packages' drive",
                    entry.path().display()
                )
            })?;
        }
    }
    for artifact in &built.artifacts {
        let target = packages.join(&artifact.file_name);
        if target.exists() {
            // Never write through the hard link into the stock package.
            fs::remove_file(&target)?;
        }
        fs::copy(built.run_directory.join(&artifact.file_name), target)?;
    }
    let mut readback = Reader::new(&packages, &output.join("readback"), false)?;
    let globals = readback
        .manager
        .lookup
        .named_tags
        .iter()
        .find(|t| t.name == "investment_globals")
        .ok_or("Native globals")?
        .hash
        .0;
    let globals = readback.tag(globals, None)?;
    let root = readback.tag(globals.u32(16)?, None)?;
    let table = readback.tag(root.u32(8 + 48 * 16)?, None)?;
    let art = readback.tag(globals.u32(16 + 66 * 16)?, None)?;
    let art_rows = art.array(8, 32, Some(0x80805DFB))?;
    let assignments = readback.tag(0x80EC3F61, None)?;
    let assigned = assignments
        .array(8, 8, None)?
        .into_iter()
        .map(|at| assignments.u32(at))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let item_rows = table.array(8, 24, Some(0x80807BE8))?;
    let hash = recipe.identity.item_hash.parse_u32()?;
    let row = item_rows
        .iter()
        .copied()
        .find(|&r| table.u32(r).ok() == Some(hash))
        .ok_or("Imported weapon is absent")?;
    let definition = readback.tag(table.u32(row + 16)?, Some(0x80807BEA))?;
    let mut weapon_keys = BTreeSet::new();
    for selection in definition.array(definition.pointer(0x88)?, 4, Some(0x808077B5))? {
        let row = art_rows[usize::from(definition.u16(selection + 2)?)];
        weapon_keys.extend(row_keys(&art, row)?);
    }
    let sockets = definition.array(definition.pointer(0x68)?, 0x50, Some(0x808077C4))?;
    let row = sockets[socket];
    let initial = item_rows[usize::from(definition.u16(row + 2)?)];
    assert_eq!(table.u32(initial)?, reset, "Socket lost its default reset");
    let choices = definition.array(row + 0x40, 0x20, Some(0x80802E03))?;
    assert_eq!(
        choices.len(),
        links.len() + 1,
        "The socket must offer only the reset and the converted ornaments"
    );
    let mut seen = BTreeSet::new();
    for (index, at) in choices.into_iter().enumerate() {
        let item = item_rows[usize::from(definition.u16(at)?)];
        let plug = table.u32(item)?;
        if index == 0 {
            assert_eq!(plug, reset, "The reset must stay the first choice");
            continue;
        }
        assert_ne!(plug, reset);
        assert_eq!(
            definition.u64(at + 8)?,
            0,
            "Ornament has an unlock condition"
        );
        let keys = plug_keys(&mut readback, table.u32(item + 16)?, &art, &art_rows)?;
        assert!(!keys.is_empty(), "Ornament {plug:08X} has no art");
        assert!(
            keys.iter().all(|key| assigned.contains(key)),
            "Ornament {plug:08X} art has no assignment"
        );
        assert!(
            keys.is_disjoint(&weapon_keys),
            "Ornament {plug:08X} reuses the weapon's own art"
        );
        assert!(seen.insert(keys), "Two ornaments share their art");
        println!("ornament choice {index}: {plug:08X}");
    }
    readback.finish()?;
    Ok(())
}
