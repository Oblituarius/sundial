//! Source-only exports the D1 tests and inspection tools read. Upstream removed these from
//! `Source` when the importer moved its reports into the catalog; they are kept here, apart from
//! upstream's files, for the PS4 gate test and `examples/test_d1_roi_import.rs`.
use super::*;
use serde_json::json;

impl Source {
    /// Write the package inspection report as `inspection.json` in a fresh `output`.
    pub fn write_inspection(&self, output: &Path) -> Result<PathBuf> {
        let output = self.output(output)?;
        fs::create_dir_all(&output)?;
        let path = output.join("inspection.json");
        crate::io::write_json(&path, &serde_json::to_value(&self.report)?)?;
        Ok(path)
    }

    /// Export both body variants, component owners, model streams and texture plates.
    /// Material execution, cloth simulation and native D2 conversion remain separate work.
    pub fn export_armor(&self, item: u32, output: &Path) -> Result<PathBuf> {
        let output = self.output(output)?;
        let armor = self
            .armor()?
            .into_iter()
            .find(|armor| armor.hash == item)
            .context("Selected source armor is missing")?;
        let mut assets = BTreeMap::new();
        let bodies = self.armor_bodies(&armor, &mut assets)?;
        let ornaments = self.ornaments(&armor, &mut assets)?;
        let dyes = self.dyes(armor.definition, &mut assets)?;
        let talents = self.item_talents(armor.definition, &mut assets)?;
        self.unchanged()?;
        fs::create_dir_all(&output)?;
        let assets = self.write_assets(&output, assets)?;
        let path = output.join("armor.json");
        crate::io::write_json(
            &path,
            &json!({"source":"d1_roi","native_conversion":false,"item":armor,
            "builds":self.report.builds,"package_stamp":self.report.stamp,"bodies":bodies,"ornaments":ornaments,"dyes":dyes,"talents":talents,"assets":assets,
            "limits":["Source armor, ornaments, material programs and texture bytes are preserved. Material execution, body skinning, effects and cloth still need native D2 translation and gameplay verification."]}),
        )?;
        Ok(path)
    }
}
