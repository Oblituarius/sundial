//! Prepare native armor and its linked private appearance choices as one recipe.
use super::*;
use crate::GraphReference;

fn hash(text: &str) -> u32 {
    text.bytes().fold(0x811C9DC5u32, |h, b| {
        h.wrapping_mul(16777619) ^ u32::from(b)
    })
}

fn number(value: &Value) -> Result<u32> {
    u32::try_from(value.as_u64().context("Armor identity missing")?).map_err(Into::into)
}

fn bucket(slot: u8) -> Result<u32> {
    match slot {
        1 => Ok(3448274439),
        2 => Ok(3551918588),
        3 => Ok(14239492),
        4 => Ok(20886954),
        5 => Ok(1585787867),
        _ => anyhow::bail!("Unknown source armor slot"),
    }
}

impl Assembly<'_> {
    #[allow(clippy::too_many_arguments)]
    fn armor_graph(
        &mut self,
        item: &Armor,
        identity: u32,
        donor: u32,
        template_index: u16,
        bodies: &[Value],
        dyes: &[Value],
        ornaments: &[Value],
    ) -> Result<GraphReference> {
        ensure!(
            bodies.len() == 2
                && bodies[0]["body"] == "masculine"
                && bodies[1]["body"] == "feminine",
            "Armor body selector order differs"
        );
        let mut parts = Vec::new();
        let mut singles = Vec::new();
        for (index, body) in bodies.iter().enumerate() {
            let symbol = self.body(body, dyes, &format!("body-{index}"))?;
            let assignment = number(&body["assignment"])?;
            parts.push(json!({"source_assignment":assignment,"key":hash(&format!("parhelion/imported-gear/{identity:08X}/art-{index}")),"parent":symbol}));
            singles.push(assignment);
        }
        let icon = self.icon(item)?;
        let (native_dyes, dye_rows) = self.armor_dyes(dyes, donor, identity)?;
        let mut limitations = [
            "Native rendering, cloth motion and repeated ornament selection require in-game acceptance.",
            "Source effect sequences and their particle events are not translated.",
        ]
        .map(String::from)
        .to_vec();
        limitations.extend(self.approximations.iter().cloned());
        let graph = json!({"kind":"armor","source":"d1_roi","source_item":item.hash,"source_name":item.name,"source_icon_png":icon,
            "collection_set":item.collection_set,
            "source_class":item.class_type,"source_bucket":bucket(item.slot)?,"source_rarity":item.rarity,
            "item_hash":identity,"native_item":donor,"converter_revision":crate::graph::CONVERTER_REVISION,
            "source_builds":self.source.report.builds,"source_stamp":self.source.report.stamp,"source_ornaments":ornaments,
            "nodes":self.nodes,"parent":parts[0]["parent"],"dyes":native_dyes,"dye_rows":[[],dye_rows,[]],
            "gear_art":{"selector_format":"shadowkeep","rows":[{"class":item.class_type,"flags":0,"template_index":template_index,"singles":singles,"slots":[]}],"parts":parts},
            "collections_unconditional":true,"rendering":self.evidence,"installable":true,"gameplay_verified":false,
            "limitations":limitations});
        crate::io::write_json(&self.folder.join("asset-graph.json"), &graph)?;
        self.native.finish()?;
        GraphReference::new(&self.folder, identity)
    }
}

/// The Stat Allocation sockets of Shadowkeep armor, as its exotic chests carry them (Hallowfire
/// Heart 471A10E6): socket type, randomized plug set and the set's even split. Top set 413 holds
/// Mobility, Resilience and Recovery combinations, Bottom set 414 Discipline, Intellect and
/// Strength, 64 plugs each totalling 14 to 17 points. Each stock socket selects from its set
/// with the one-instruction program `[11 0]`.
const STAT_ALLOCATION: [(u16, u16, u32); 4] = [
    (760, 413, 0x8D7F_89DB),
    (761, 413, 0x8D7F_89DB),
    (762, 414, 0x6BB6_C22D),
    (763, 414, 0x6BB6_C22D),
];

/// Shadowkeep armor's socket order up to its stat sockets: the perk sockets, the intrinsic, then
/// the shader. A Rise of Iron exotic's talent columns after its first become the perk sockets in
/// column order and its signature talent the intrinsic, so the item reads as Shadowkeep armor does
/// rather than as the donor's year-two layout. The donor's other sockets, such as its armor mod
/// and tier, have no source counterpart; see `disable_remaining`. Talent choices follow their
/// columns to the new lanes.
fn armor_order(native: &Value, columns: &[Value], variants: &mut [Value]) -> Result<Vec<Value>> {
    let sockets = native["sockets"].as_array().context("Native sockets")?;
    let find = |role: fn(&[Value]) -> bool| {
        sockets.iter().position(|socket| {
            socket["choices"]
                .as_array()
                .is_some_and(|choices| !choices.is_empty() && role(choices))
        })
    };
    let intrinsic = find(|choices| {
        choices
            .iter()
            .all(|c| c["category"].as_u64() == Some(perk::INTRINSIC_CATEGORY))
    })
    .context("Native armor has no intrinsic socket")?;
    let shader = find(|choices| {
        choices
            .iter()
            .any(|c| c["default"] == true && c["item_kind"] == perk::SHADER_ITEM_KIND)
    })
    .context("Native armor has no shader socket")?;
    let mut lanes = Vec::new();
    for variant in variants.iter() {
        let lane = usize::try_from(variant["socket_index"].as_u64().context("Talent socket")?)?;
        if lane != intrinsic && !lanes.contains(&lane) {
            lanes.push(lane);
        }
    }
    lanes.extend([intrinsic, shader]);
    let mut ordered = Vec::new();
    for (position, &lane) in lanes.iter().enumerate() {
        let mut column = columns.get(lane).cloned().unwrap_or(Value::Null);
        // A talent column past the donor's sockets already names its own socket type.
        let Some(socket) = sockets.get(lane) else {
            ordered.push(column);
            continue;
        };
        if column.is_null() && position != lane {
            // A donor socket the layout moves keeps its own choices, default first.
            let choices = socket["choices"]
                .as_array()
                .context("Native socket choices")?;
            let hashes = choices
                .iter()
                .filter(|c| c["default"] == true)
                .chain(choices.iter().filter(|c| c["default"] != true))
                .map(|c| Ok(format!("0x{:08X}", number(&c["hash"])?)))
                .collect::<Result<Vec<_>>>()?;
            column = json!({"choices":hashes});
        }
        if !column.is_null() && column.get("socket_type").is_none() {
            column["socket_type"] = socket["socket_type"].clone();
        }
        ordered.push(column);
    }
    for variant in variants.iter_mut() {
        let lane = usize::try_from(variant["socket_index"].as_u64().context("Talent socket")?)?;
        let position = lanes
            .iter()
            .position(|&l| l == lane)
            .context("Talent socket left the armor layout")?;
        variant["socket_index"] = json!(position);
    }
    Ok(ordered)
}

/// Disable the donor sockets past the finished layout, so none of its own plugs remain.
fn disable_remaining(native: &Value, columns: &mut Vec<Value>) -> Result<()> {
    let count = native["sockets"]
        .as_array()
        .context("Native sockets")?
        .len();
    while columns.len() < count {
        columns.push(json!({"choices":[],"socket_type":u16::MAX}));
    }
    Ok(())
}

impl Source {
    /// Convert both source bodies and linked ornaments into a portable native recipe.
    pub fn prepare_armor(
        &self,
        item: u32,
        native: &Path,
        donors: &Value,
        output: &Path,
        options: &NativeOptions,
        progress: &mut dyn FnMut(String),
    ) -> Result<PathBuf> {
        progress("Verifying source integrity".to_owned());
        let output = self.output(output)?;
        let output = crate::io::outside(
            &output,
            native.parent().context("Native installation root")?,
        )?;
        progress(format!("Reading investment tables for {item:08X}"));
        let armor = self
            .armor()?
            .into_iter()
            .find(|a| a.hash == item)
            .context("Selected source armor is missing")?;
        progress(format!("Reading body components for {}", armor.name));
        let mut assets = BTreeMap::new();
        let bodies = self.armor_bodies(&armor, &mut assets)?;
        progress(format!("Reading ornaments for {}", armor.name));
        let ornaments = self.ornaments(&armor, &mut assets)?;
        progress(format!("Reading dye data for {}", armor.name));
        let dyes = self.dyes(armor.definition, &mut assets)?;
        let talents = self.item_talents(armor.definition, &mut assets)?;
        progress(format!(
            "Source: {} body variants, {} ornaments, {} dye sets, {} assets",
            bodies.len(),
            ornaments.len(),
            dyes.len(),
            assets.len()
        ));
        let source = json!({"source":"d1_roi","item":armor,"talents":talents,
            "builds":self.report.builds,"package_stamp":self.report.stamp});
        fs::create_dir_all(output.join("source"))?;
        crate::io::write_json(&output.join("source/armor.json"), &source)?;
        let namespace = options
            .namespace
            .clone()
            .unwrap_or_else(|| crate::graph::import_namespace("d1_roi", &armor.name, item));
        let identity = crate::graph::import_identity(&namespace)?;
        let private_item = u32::from_str_radix(
            identity["item_hash"]
                .as_str()
                .context("Native identity")?
                .trim_start_matches("0x"),
            16,
        )?;
        progress("Opening native D2 packages".to_owned());
        let mut assembly = Assembly::new(self, native, &output.join("graph"), options, None)?;
        progress("Searching for native armor donor".to_owned());
        let mut selected = None;
        for donor in donors["gear"]
            .as_array()
            .context("Native gear donors are missing")?
        {
            if donor["bucket_hash"].as_u64() != Some(u64::from(bucket(armor.slot)?))
                || donor["class_type"].as_u64() != Some(u64::from(armor.class_type))
                || donor["collection_backed"] != true
            {
                continue;
            }
            let hash = number(&donor["hash"])?;
            let tag = crate::tiger::shadowkeep::item_tag(&mut assembly.native, hash)?;
            let definition = assembly.native.tag(tag, Some(0x80807BEA))?;
            if definition.u8(0xBA)? == 5 {
                continue;
            }
            let selectors = definition.array(definition.pointer(0x88)?, 4, Some(0x808077B5))?;
            let Some(selector) = selectors.first() else {
                continue;
            };
            let sockets = crate::tiger::shadowkeep::sockets(&mut assembly.native, hash)?;
            let socket = sockets["sockets"]
                .as_array()
                .context("Native socket report")?
                .iter()
                .find(|s| {
                    s["choices"].as_array().is_some_and(|choices| {
                        choices.iter().any(|c| {
                            c["default"] == true
                                && c["item_kind"] == 13
                                && c["art_indices"].as_array().is_some_and(Vec::is_empty)
                        }) && choices
                            .iter()
                            .filter(|c| {
                                c["default"] != true
                                    && c["item_kind"] == 13
                                    && c["art_indices"].as_array().is_some_and(|a| !a.is_empty())
                            })
                            .count()
                            >= ornaments.len()
                    })
                })
                .cloned();
            if !ornaments.is_empty() && socket.is_none() {
                continue;
            }
            selected = Some((donor.clone(), definition.u16(selector + 2)?, socket));
            break;
        }
        let (donor, template, socket) = selected.context(
            "No native armor template has the required class, slot and appearance choices",
        )?;
        let donor_hash = number(&donor["hash"])?;
        progress(format!("Donor: {donor_hash:08X}"));
        let (columns, variants, perks, damage) =
            perk::translate(&mut assembly, &source, donor_hash, perk::Host::Armor)?;
        ensure!(
            damage.is_none(),
            "Armor talents cannot set the item's damage type"
        );
        let mut variants = variants
            .as_array()
            .context("Armor talent variants")?
            .clone();
        let native_sockets = crate::tiger::shadowkeep::sockets(&mut assembly.native, donor_hash)?;
        let mut columns = armor_order(
            &native_sockets,
            columns.as_array().context("Armor talent columns")?,
            &mut variants,
        )?;
        // Shadowkeep armor rolls its stats from two Top and two Bottom Stat Allocation sockets.
        // Rise of Iron rolled Intellect, Discipline and Strength from the item's stat budget,
        // which no source record fixes as numbers, so each socket starts on its set's even split.
        for &(socket_type, set, plug) in &STAT_ALLOCATION {
            crate::tiger::shadowkeep::item_tag(&mut assembly.native, plug)
                .with_context(|| format!("Stat allocation plug {plug:08X}"))?;
            columns.push(
                json!({"choices":[format!("0x{plug:08X}")],"socket_type":socket_type,
                "randomized_plug_set_index":set,
                "randomized_selection_program":[{"opcode":11,"operand":0}]}),
            );
        }
        progress(format!("Converting {}", armor.name));
        let mut reference = assembly.armor_graph(
            &armor,
            private_item,
            donor_hash,
            template,
            &bodies,
            &dyes,
            &ornaments,
        )?;
        if let Some(socket) = socket.filter(|_| !ornaments.is_empty()) {
            // The ornament socket closes Shadowkeep armor's layout, after the stat sockets.
            let socket_index = columns.len();
            columns.push(Value::Null);
            let candidates = socket["choices"]
                .as_array()
                .context("Native appearance choices")?;
            let reset = candidates
                .iter()
                .find(|c| c["default"] == true)
                .context("Native appearance reset missing")?;
            let reset = number(&reset["hash"])?;
            let candidates = candidates
                .iter()
                .filter(|c| {
                    c["default"] != true
                        && c["item_kind"] == 13
                        && c["art_indices"].as_array().is_some_and(|a| !a.is_empty())
                })
                .collect::<Vec<_>>();
            let mut choices = vec![json!(format!("0x{reset:08X}"))];
            for (index, ornament) in ornaments.iter().enumerate() {
                let info = &ornament["item"];
                let mut source_item = Armor {
                    hash: number(&info["hash"])?,
                    name: info["name"].as_str().context("Ornament name")?.to_owned(),
                    kind: armor.kind.clone(),
                    class_type: armor.class_type,
                    slot: armor.slot,
                    rarity: armor.rarity,
                    collection_set: None,
                    definition: number(&info["definition"])?,
                    strings: number(&info["strings"])?,
                    arrangements: Vec::new(),
                };
                let choice = index + 1;
                let plug = number(&candidates[index]["hash"])?;
                let target = hash(&format!(
                    "parhelion/{namespace}/socket/{socket_index}/choice/{choice}/private-plug"
                ));
                let mut ornament_assembly = Assembly::new(
                    self,
                    native,
                    &output.join(format!("ornament-{index}")),
                    options,
                    None,
                )?;
                let (name, icon_container) =
                    ornament_assembly.ornament_display(&armor, ornament)?;
                if !name.is_empty()
                    && super::super::localization::Labels::new(self)?
                        .text(self, &self.read(source_item.strings)?, 0x78)?
                        .is_empty()
                {
                    source_item.name = name;
                }
                // An appearance item may override only some of its parent's
                // dye slots. Retain the remaining slots and layer precedence.
                let mut ornament_dyes = dyes.clone();
                ornament_dyes.extend(self.dyes(source_item.definition, &mut assets)?);
                ornament_dyes.sort_by_key(|dye| match dye["layer"].as_str() {
                    Some("default") => 0,
                    Some("custom") => 1,
                    Some("fixed") => 2,
                    _ => unreachable!("Source::dyes declares the layer names"),
                });
                let graph = ornament_assembly.armor_graph(
                    &source_item,
                    target,
                    donor_hash,
                    template,
                    ornament["bodies"]
                        .as_array()
                        .context("Ornament body selectors")?,
                    &ornament_dyes,
                    &[],
                )?;
                let manifest = graph.directory.join("asset-graph.json");
                let mut value: Value = serde_json::from_slice(&fs::read(&manifest)?)?;
                let icon = if let Some(icon) = value["source_icon_png"].as_str() {
                    Some(icon.to_owned())
                } else {
                    ornament_assembly.container_icon(icon_container)?
                };
                value["ornament_icon_png"] = json!(icon);
                value["rendering"] = json!(ornament_assembly.evidence);
                value["ornament"] = json!({"target_item":private_item,"target_socket":socket_index,"choice_index":choice,"native_plug":plug,"default_reset":reset,"name":source_item.name});
                crate::io::write_json(&manifest, &value)?;
                reference
                    .attachments
                    .push(GraphReference::new(&graph.directory, private_item)?);
                choices.push(json!(format!("0x{plug:08X}")));
                variants.push(json!({"socket_index":socket_index,"choice_index":choice,"source_plug_hash":format!("0x{plug:08X}"),"name":source_item.name,"description":"Changes this armor's appearance. Select the default ornament to restore its original appearance.","replace_effects":true,"sandbox_perks":[]}));
            }
            columns[socket_index] = json!({"choices":choices,"socket_type":socket["socket_type"],"choice_conditions":[],"choice_weight_bits":[],"randomized_selection_program":[]});
        }
        disable_remaining(&native_sockets, &mut columns)?;
        let manifest = reference.directory.join("asset-graph.json");
        let mut graph: Value = serde_json::from_slice(&fs::read(&manifest)?)?;
        graph["source_talents"] = talents;
        graph["perk_translation"] = perks;
        graph["limitations"].as_array_mut().context("Armor limitations")?.extend([
            json!("Supported armor talents use private native effects. Equipping, event delivery, repeated use, selection changes and cleanup require in-game acceptance."),
            json!("Native base stats remain the baseline. Source armor stat tiers, ability grants, progression and unsupported talent effects are not translated."),
        ]);
        crate::io::write_json(&manifest, &graph)?;
        let attachments = std::mem::take(&mut reference.attachments);
        reference = GraphReference::new(&reference.directory, private_item)?;
        reference.attachments = attachments;
        reference.validate_reusable("armor")?;
        self.unchanged()?;
        let rarity = match armor.rarity {
            1 => "common",
            2 => "uncommon",
            3 => "rare",
            4 => "legendary",
            5 => "exotic",
            _ => anyhow::bail!("Unknown source armor rarity"),
        };
        let flavor = super::super::localization::Labels::new(self)?.text(
            self,
            &self.read(armor.strings)?,
            0x88,
        )?;
        let mut recipe = json!({"schema":1,"kind":"armor","collection_placement":"sunrise_badge","namespace":namespace,"identity":identity,"name":armor.name,"type_name":armor.kind,"flavor":flavor,
            "donor":{"item_hash":format!("0x{donor_hash:08X}"),"expected_name":donor["name"]},"source":"Source: Rise of Iron",
            "overrides":{"rarity":rarity,"imported_graph":reference,"base_sandbox_perks":[],"socket_columns":columns,"socket_plug_variants":variants,"remove_lore":true}});
        crate::artwork::apply(&mut recipe, &output.join("graph"))?;
        let path = output.join("armor.parhelion.json");
        crate::io::write_json(&path, &recipe)?;
        Ok(path)
    }
}
