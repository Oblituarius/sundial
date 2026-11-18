//! Stock-only package reads, so an import never reads what Sundial built into the installation.
//!
//! Sundial installs its builds into the stock `packages` folder: new packages, and patch files one
//! number above the stock patch of each package it changes. The game and every package reader take
//! the highest patch, so a reader of that folder sees the last build's tables and tags, and an
//! import could pick a tag of that build or refuse a name the build already uses. Every package
//! Sundial writes carries its build signature at header `+8` (`parhelion::SUNDIAL_BUILD_SIGNATURE`),
//! and the stock patches stay beside Sundial's.
//!
//! Inside a [`scope`], a reader of that folder swaps each signed package for the highest unsigned
//! patch of the same package, drops packages with none, and rebuilds its tag index from them. It
//! all happens in memory: nothing is written to the installation. A folder without signed packages
//! reads as it is.
use anyhow::Result;
use std::{
    cell::RefCell,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use tiger_pkg::{PackageManager, manager::PackagePath};

/// `parhelion::SUNDIAL_BUILD_SIGNATURE`, the header word of every package Sundial writes.
const BUILD_SIGNATURE: u64 = 0x1236_7498_D1CB_BA35;

thread_local! {
    /// Package folders this thread's readers read stock-only, innermost last.
    static SCOPES: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// Readers this thread opens on `packages` read it stock-only until the scope drops.
pub(crate) struct Scope(PathBuf);

impl Drop for Scope {
    fn drop(&mut self) {
        SCOPES.with(|scopes| {
            let mut scopes = scopes.borrow_mut();
            if let Some(at) = scopes.iter().rposition(|folder| *folder == self.0) {
                scopes.remove(at);
            }
        });
    }
}

fn canonical(packages: &Path) -> PathBuf {
    packages
        .canonicalize()
        .unwrap_or_else(|_| packages.to_owned())
}

pub(crate) fn scope(packages: &Path) -> Scope {
    let folder = canonical(packages);
    SCOPES.with(|scopes| scopes.borrow_mut().push(folder.clone()));
    Scope(folder)
}

/// Whether readers this thread opens on `packages` read it stock-only.
pub(crate) fn wanted(packages: &Path) -> bool {
    let folder = canonical(packages);
    SCOPES.with(|scopes| scopes.borrow().contains(&folder))
}

fn signed(path: &Path) -> bool {
    let mut header = [0; 16];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok()
        && u64::from_le_bytes(header[8..].try_into().unwrap_or_default()) == BUILD_SIGNATURE
}

/// The highest unsigned patch of the package `path` names a patch of, such as stock
/// `w64_globals_03ab_6.pkg` for Sundial's `w64_globals_03ab_7.pkg`.
fn stock_patch(path: &Path) -> Result<Option<PathBuf>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase();
    let Some((stem, _)) = name.strip_suffix(".pkg").and_then(|n| n.rsplit_once('_')) else {
        return Ok(None);
    };
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut best: Option<(u32, PathBuf)> = None;
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let other = entry.file_name().to_string_lossy().to_lowercase();
        let Some(patch) = other
            .strip_suffix(".pkg")
            .and_then(|n| n.rsplit_once('_'))
            .filter(|(s, _)| *s == stem)
            .and_then(|(_, patch)| patch.parse::<u32>().ok())
        else {
            continue;
        };
        if best.as_ref().is_some_and(|(b, _)| *b >= patch) || signed(&entry.path()) {
            continue;
        }
        best = Some((patch, entry.path()));
    }
    Ok(best.map(|(_, path)| path))
}

/// Makes `manager` read its folder stock-only. Returns whether anything changed.
pub(crate) fn filter(manager: &mut PackageManager) -> Result<bool> {
    let mut changed = false;
    let ids = manager.package_paths.keys().copied().collect::<Vec<_>>();
    for id in ids {
        let path = PathBuf::from(&manager.package_paths[&id].path);
        if !signed(&path) {
            continue;
        }
        changed = true;
        match stock_patch(&path)? {
            Some(stock) => {
                manager.package_paths.insert(
                    id,
                    PackagePath::parse_with_defaults(&stock.to_string_lossy()),
                );
            }
            None => {
                manager.package_paths.remove(&id);
            }
        }
    }
    if changed {
        manager.build_lookup_tables();
    }
    Ok(changed)
}

/// A Sundial package manager reading what `reader` reads: stock-only when the reader is.
pub(crate) fn sundial_manager(
    reader: &super::reader::Reader,
) -> Result<sundial::package_authoring::PackageManager> {
    let source = &reader.manager;
    let mut manager = sundial::package_authoring::PackageManager::new(
        &source.package_dir,
        source.version,
        Some(source.platform),
    )
    .map_err(anyhow::Error::msg)?;
    if reader.is_stock() {
        let lookup = &source.lookup;
        manager.package_paths = source
            .package_paths
            .iter()
            .map(|(id, path)| (*id, path.clone()))
            .collect();
        manager.lookup = tiger_pkg::manager::TagLookupIndex {
            tag32_entries_by_pkg: lookup.tag32_entries_by_pkg.clone(),
            tag64_entries: lookup.tag64_entries.clone(),
            tag32_to_tag64: lookup.tag32_to_tag64.clone(),
            named_tags: lookup.named_tags.clone(),
            redaction_levels: lookup.redaction_levels.clone(),
        };
    }
    Ok(manager)
}
