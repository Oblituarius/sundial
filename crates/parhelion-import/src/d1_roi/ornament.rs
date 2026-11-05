//! Resolve armor and weapon appearance choices through their original talent grid.
use super::*;
use serde_json::{Value, json};

/// One appearance item a talent grid step offers.
struct Appearance {
    grid: u32,
    node: usize,
    step: usize,
    /// Offset of the step's appearance entry in the grid.
    offset: usize,
    /// Ordinal in the item definition and string tables.
    index: usize,
    definition_table: u32,
    hash: u32,
    tag: u32,
    data: Payload,
    text_tag: u32,
    text: Payload,
}

impl Source {
    /// Every appearance item an item's talent grid offers, in grid order, with the tables it
    /// read added to `assets`.
    fn appearances(
        &self,
        definition: u32,
        assets: &mut BTreeMap<u32, Payload>,
    ) -> Result<Vec<Appearance>> {
        let definition = self.read(definition)?;
        if definition.u64(0x58)? == 0 {
            return Ok(Vec::new());
        }
        let talent = definition.pointer(0x58)?;
        ensure!(
            talent >= 4 && definition.u32(talent - 4)? == 0x808017BD,
            "Unknown item talent record"
        );
        let ordinal = usize::from(definition.u16(talent)?);
        if ordinal == usize::from(u16::MAX) {
            return Ok(Vec::new());
        }
        let (table_tag, table) = self.table(0x808018C2)?;
        let rows = table.array(8, 24, Some(0x808017CB))?;
        let grid_tag = table.u32(*rows.get(ordinal).context("Talent grid ordinal")? + 16)?;
        let grid = self.read(grid_tag)?;
        let (definitions_tag, definitions) = self.table(0x80801933)?;
        let definitions_rows = definitions.array(8, 24, Some(0x808016BD))?;
        let (strings_tag, strings) = self.table(0x808034C7)?;
        let string_rows = strings.array(8, 16, Some(0x808033E8))?;
        let mut result = Vec::new();
        for (node, row) in grid.array(0x10, 64, None)?.into_iter().enumerate() {
            for (step, row) in grid.array(row + 24, 144, None)?.into_iter().enumerate() {
                for appearance in grid.array(row + 0x70, 4, Some(0x80800F94))? {
                    let index = usize::from(grid.u16(appearance + 2)?);
                    if index == usize::from(u16::MAX) {
                        continue;
                    }
                    let row = *definitions_rows
                        .get(index)
                        .context("Ornament item ordinal")?;
                    let hash = definitions.u32(row)?;
                    let tag = definitions.u32(row + 16)?;
                    let data = self.read(tag)?;
                    ensure!(
                        self.reference(tag)? == 0x80801806 && data.u32(0x78)? == hash,
                        "Ornament identity mismatch"
                    );
                    let gear = data.pointer(0x60)?;
                    ensure!(
                        gear >= 4 && data.u32(gear - 4)? == 0x80801020,
                        "Ornament has no appearance record"
                    );
                    let text_tag = strings
                        .u32(*string_rows.get(index).context("Ornament strings ordinal")? + 8)?;
                    let text = self.read(text_tag)?;
                    result.push(Appearance {
                        grid: grid_tag,
                        node,
                        step,
                        offset: appearance,
                        index,
                        definition_table: definitions_tag,
                        hash,
                        tag,
                        data,
                        text_tag,
                        text,
                    });
                }
            }
        }
        for (tag, data) in [
            (table_tag, table),
            (grid_tag, grid),
            (definitions_tag, definitions),
            (strings_tag, strings),
        ] {
            assets.insert(tag, data);
        }
        Ok(result)
    }

    pub(super) fn ornaments(
        &self,
        armor: &Armor,
        assets: &mut BTreeMap<u32, Payload>,
    ) -> Result<Vec<Value>> {
        let mut labels = super::localization::Labels::new(self)?;
        let mut result = Vec::new();
        for choice in self.appearances(armor.definition, assets)? {
            let data = &choice.data;
            let gear = data.pointer(0x60)?;
            let arrangements = data
                .array(gear, 4, Some(0x8080101A))?
                .into_iter()
                .filter_map(|row| {
                    let class = data.u8(row).ok()? as i8;
                    (class == -1 || class == armor.class_type as i8).then(|| data.u16(row + 2))
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                !arrangements.is_empty() && !arrangements.contains(&u16::MAX),
                "Ornament has no compatible art arrangement"
            );
            let source_name = labels.text(self, &choice.text, 0x78)?;
            let item = Armor {
                collection_set: None,
                hash: choice.hash,
                name: if source_name.is_empty() {
                    format!("{} Ornament", armor.name)
                } else {
                    source_name
                },
                kind: labels.text(self, &choice.text, 0x80)?.trim().to_owned(),
                class_type: armor.class_type,
                slot: armor.slot,
                rarity: data.u8(0x8A)?,
                definition: choice.tag,
                strings: choice.text_tag,
                arrangements,
            };
            let bodies = self.armor_bodies(&item, assets)?;
            result.push(json!({"target_armor":armor.hash,"grid":choice.grid,"node":choice.node,"step":choice.step,
                "appearance_offset":choice.offset,"item_ordinal":choice.index,"definition_table":choice.definition_table,"item":item,"bodies":bodies}));
            assets.insert(choice.tag, choice.data);
            assets.insert(choice.text_tag, choice.text);
        }
        Ok(result)
    }

    /// A weapon's ornaments with their selectable model art. A grid can offer one ornament
    /// from several steps, which become one choice at its first step.
    pub(super) fn weapon_ornaments(
        &self,
        weapon: &Weapon,
        assets: &mut BTreeMap<u32, Payload>,
    ) -> Result<Vec<Value>> {
        let mut labels = super::localization::Labels::new(self)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut result = Vec::new();
        for choice in self.appearances(weapon.definition, assets)? {
            if !seen.insert(choice.hash) {
                continue;
            }
            let data = &choice.data;
            let gear = data.pointer(0x60)?;
            // Weapon art is class-neutral, as for the weapon itself.
            let mut arrangements = Vec::new();
            for row in data.array(gear, 4, Some(0x8080101A))? {
                let index = data.u16(row + 2)?;
                if data.u8(row)? == 255 && index != u16::MAX {
                    arrangements.push(index);
                }
            }
            ensure!(
                !arrangements.is_empty(),
                "Weapon ornament {:08X} has no class-neutral art arrangement",
                choice.hash
            );
            let source_name = labels.text(self, &choice.text, 0x78)?;
            let item = Weapon {
                hash: choice.hash,
                name: if source_name.is_empty() {
                    format!("{} Ornament", weapon.name)
                } else {
                    source_name
                },
                kind: weapon.kind.clone(),
                flavor: labels
                    .optional_text(self, &choice.text, 0x88)?
                    .unwrap_or_default(),
                slot: weapon.slot,
                rarity: data.u8(0x8A)?,
                definition: choice.tag,
                strings: choice.text_tag,
                pattern: weapon.pattern,
                arrangements,
            };
            let art = self
                .weapon_art(&item, assets)
                .with_context(|| format!("Weapon ornament {:08X} art", choice.hash))?;
            result.push(json!({"target_weapon":weapon.hash,"grid":choice.grid,"node":choice.node,"step":choice.step,
                "appearance_offset":choice.offset,"item_ordinal":choice.index,"definition_table":choice.definition_table,"item":item,"art":art}));
            assets.insert(choice.tag, choice.data);
            assets.insert(choice.text_tag, choice.text);
        }
        Ok(result)
    }
}
