//! Bounded source action lowering. An unsupported node rejects the entire choice.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use sundial::package_authoring::sandbox_perk::program::{
    Action, AmmunitionStore, AmmunitionTarget, DamageMode, NativeNode, Program, Trigger,
};
mod armor;
mod audio;
mod condition;
mod damage;
mod direct;
mod energy;
mod feedback;
mod filter;
mod grant;
mod hit;
mod hud;
mod incoming;
/// Object-label filters, shared with query components.
pub(in crate::d1_roi::native) mod label;
mod orb;
mod pattern;
mod policy;
mod record;
mod scalar;
mod timer;
mod transfer;
mod weighted;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Host {
    Weapon,
    Armor,
}

impl Host {
    fn validate_variant(self, step: &Value, record: &Payload) -> Result<()> {
        if self == Self::Armor {
            ensure!(
                step["appearance_choices"].as_u64() == Some(0),
                "Appearance choices use the separate ornament route"
            );
            ensure!(
                record.bytes::<40>(48)? == [0; 40],
                "Source armor progression or variant metadata requires translation"
            );
        }
        Ok(())
    }

    fn validate_action(self, source: &Source, tag: u32) -> Result<()> {
        self.validate_action_inner(source, tag, &mut Vec::new())
    }

    fn validate_action_inner(
        self,
        source: &Source,
        tag: u32,
        ancestors: &mut Vec<u32>,
    ) -> Result<()> {
        if self != Self::Armor {
            return Ok(());
        }
        ensure!(
            ancestors.len() < 8 && !ancestors.contains(&tag),
            "Source armor controllers form a cycle or exceed the depth limit"
        );
        ancestors.push(tag);
        let result = self.validate_action_body(source, tag, ancestors);
        ancestors.pop();
        result
    }

    fn validate_action_body(
        self,
        source: &Source,
        tag: u32,
        ancestors: &mut Vec<u32>,
    ) -> Result<()> {
        if self == Self::Armor {
            let data = source.read(tag)?;
            for row in data.array(0x38, 8, None)? {
                let at = data.pointer(row)?;
                match data.u8(at)? {
                    0 => pattern::armor_attachment_inner(source, &data, at, ancestors)?,
                    // The qualified direct health response resolves the owning player.
                    // Profile admission still checks the complete zero-damage envelope.
                    3 => ensure!(
                        data.u8(at + 2)? == 1,
                        "Armor direct effects require the player target"
                    ),
                    // Explicit player properties use the same target resolver on armor.
                    // An item-local property or selected ability needs a separate contract.
                    9 => ensure!(
                        data.u8(at + 2)? == 1 && data.u32(at + 4)? == 0,
                        "Source armor property requires an unmasked player target"
                    ),
                    // Ammo modifier callbacks resolve the action owner's player component,
                    // independently of an owning weapon or a triggering-weapon event.
                    4 | 7 | 10 | 11 | 12 | 19 | 27 | 28 => {}
                    // The aim-suspension record registers on the action owner's player, as
                    // Tome of Dawn's does from armor.
                    31 => {}
                    // Proportional refills and reserve transfers resolve the player's
                    // inventory. Armor cannot supply an owning weapon amount.
                    14 | 15 => ensure!(
                        data.u32(at + 8)? == 0,
                        "Source armor refill requires an explicit inventory slot"
                    ),
                    // Literal damage rows consume the event buffers directly. Without
                    // the owner gate they need no weapon supplied by the armor host.
                    34 => ensure!(
                        data.u8(at + 0x94)? == 0,
                        "Source armor damage modifier requires the owner gate disabled"
                    ),
                    kind => anyhow::bail!("Source effect {kind} needs an armor host contract"),
                }
            }
        }
        Ok(())
    }
}

/// Equipped stat contributions of one source choice on Shadowkeep stat definitions, as recipe
/// rows and report rows. A source row is a plain delta only when both scale factors are one
/// and its selector bytes are unset.
fn adjustments(
    host: Host,
    record: &Payload,
    rows: &Value,
    definitions: &BTreeMap<u32, u16>,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let rows = rows.as_array().map(Vec::as_slice).unwrap_or_default();
    ensure!(
        rows.len() == usize::try_from(record.u64(16)?)?,
        "Source stat adjustments disagree with the variant record"
    );
    let mut stats = Vec::new();
    let mut report = Vec::new();
    let mut indices = BTreeSet::new();
    for row in rows {
        let hash = u32::try_from(row["hash"].as_u64().context("Source stat hash")?)?;
        let bytes = Payload(hex::decode(
            row["record"]
                .as_str()
                .context("Source stat adjustment bytes")?,
        )?);
        if host == Host::Armor
            && let Some(value) = budget_share(hash, &bytes)?
        {
            let index = *definitions.get(&hash).with_context(|| {
                format!("Source stat {hash:08X} has no Shadowkeep stat definition")
            })?;
            ensure!(
                indices.insert(index),
                "Source stat {hash:08X} is adjusted twice"
            );
            stats.push(json!({"definition_index":index,"value":value}));
            report.push(json!({"source_hash":hash,"source_index":row["index"],
                "definition_index":index,"budget_share":bytes.f32(4)?,
                "budget_points":ARMOR_STAT_BUDGET,"value":value}));
            continue;
        }
        ensure!(
            bytes.0.len() == 16
                && bytes.u32(4)? == 0x3F80_0000
                && bytes.u32(8)? == 0x3F80_0000
                && bytes.bytes::<3>(13)? == [0xFF, 0, 0],
            "Source stat {hash:08X} adjustment is scaled or selected and requires curve translation"
        );
        let source_value = i32::from_le_bytes(bytes.bytes::<4>(0)?);
        // Armor/Resilience keeps its investment identity across these builds. The
        // plain Armor policy treats each source unit as one native tier, expressed
        // as ten points. It does not recreate D1's health curve or armor roll.
        let scale = if host == Host::Armor {
            ensure!(
                hash == 0x1769_266F,
                "Source armor stat {hash:08X} requires a native unit and scaling contract"
            );
            10
        } else {
            1
        };
        let value = source_value
            .checked_mul(scale)
            .context("Source stat contribution overflow")?;
        // Rise of Iron and Shadowkeep share these stat hashes, including Inventory Size, which
        // the modern stat group mapping treats as repurposed, so the lookup is direct.
        let index = *definitions
            .get(&hash)
            .with_context(|| format!("Source stat {hash:08X} has no Shadowkeep stat definition"))?;
        ensure!(
            u8::try_from(index).is_ok(),
            "Source stat {hash:08X} exceeds private plug stat capacity"
        );
        ensure!(
            indices.insert(index),
            "Source stat {hash:08X} is adjusted twice"
        );
        stats.push(json!({"definition_index":index,"value":value}));
        report.push(json!({"source_hash":hash,"source_index":row["index"],
            "definition_index":index,"source_value":source_value,"unit_scale":scale,"value":value}));
    }
    Ok((stats, report))
}

/// Shadowkeep points one whole Rise of Iron armor stat budget stands for. Rise of Iron rolled
/// Intellect, Discipline and Strength as shares of a budget its quality tables set, and no source
/// record fixes it as a number. A base-roll stat takes 0.5 to 0.6 of it, so twenty points puts that
/// stat at the 10 a Shadowkeep stat holds on its Stat Allocation sockets' even split, and an
/// "Increase" talent's quarter share at 5, a minor stat mod.
const ARMOR_STAT_BUDGET: f32 = 20.0;

/// The fixed points of an armor stat row that adds a share of the item's stat budget to the stat
/// its selector names (05 Intellect, 06 Discipline, 07 Strength), as "Increase" talents do. The
/// hidden base roll instead draws each stat of a pair from a range of shares, with a pair slot in
/// place of FF, which the Stat Allocation sockets replace.
fn budget_share(hash: u32, bytes: &Payload) -> Result<Option<i32>> {
    const SELECTORS: [(u32, u8); 3] = [
        (0x089E_7467, 0x05),
        (0x6775_DCE1, 0x06),
        (0xFCFE_F4B2, 0x07),
    ];
    let Some(&(_, selector)) = SELECTORS.iter().find(|(stat, _)| *stat == hash) else {
        return Ok(None);
    };
    if bytes.0.len() != 16
        || bytes.u32(0)? != 0
        || bytes.u32(4)? != bytes.u32(8)?
        || bytes.bytes::<4>(12)? != [selector, 0xFF, 0, 0]
    {
        return Ok(None);
    }
    let share = bytes.f32(4)?;
    ensure!(
        share > 0.0 && share <= 1.0,
        "Source stat {hash:08X} budget share is out of range"
    );
    #[allow(clippy::cast_possible_truncation)]
    Ok(Some((share * ARMOR_STAT_BUDGET).round() as i32))
}

struct Lowered {
    programs: Vec<Value>,
    raw: Vec<Program>,
    tags: Vec<u32>,
    auxiliary: usize,
    stats: Vec<Value>,
    adjustments: Vec<Value>,
    /// Sandbox perks of the stock plugs that stand in for source perks, and those plugs.
    stock: Vec<u16>,
    stock_plugs: Vec<String>,
    grants: Vec<Value>,
    audio_cues: BTreeSet<u32>,
}

/// Source talent perks, by identity, that run a stock Shadowkeep perk instead of their own
/// action, on the host kind that carries it. Shadowkeep ships the same perk, while the source
/// action has no established translation:
/// - Weapon Quickdraw `4E9855BC` (`quick_draw.pattern.tft`) runs Quickdraw `2A1CBFD4` and
///   Snapshot `5D32FF95` (`snapshot.pattern.tft`) runs Snapshot Sights `39169B67`. Each source
///   pattern sets weapon component fields with no established Shadowkeep field mapping.
/// - Weapon Triple Tap `87FB6EF0` runs Triple Tap `CAB3DB58`. Both count precision hits to 3,
///   one per hit held for 2 s and capped at 3, and return 1 round. The source also attaches a
///   presentation pattern (`80A7F1F2`) the pattern translator does not admit, where the stock
///   perk attaches its own (`80BC5833`).
/// - Armor Better Already `7F10B78F` ("Shields start recovery immediately after Orb pickup")
///   runs the Better Already armor perk `12807BFF` ("Your health begins to regenerate
///   immediately after picking up an Orb of Light"), and Infusion `1ABD3D84` ("Replenishes
///   health each time you pick up an Orb of Light") runs Recuperation `CBAA6F4A`, whose text is
///   the same. Each source action attaches orb-pickup patterns of six components, two of them
///   scripted banks, with no armor host contract.
///
/// Rise of Iron and Shadowkeep share no sandbox perk identities, so the pairs are listed rather
/// than looked up.
const STOCK_EQUIVALENTS: [(Host, u32, u32); 5] = [
    (Host::Weapon, 0x4E98_55BC, 0x2A1C_BFD4),
    (Host::Weapon, 0x5D32_FF95, 0x3916_9B67),
    (Host::Weapon, 0x87FB_6EF0, 0xCAB3_DB58),
    (Host::Armor, 0x7F10_B78F, 0x1280_7BFF),
    (Host::Armor, 0x1ABD_3D84, 0xCBAA_6F4A),
];

fn conditions(
    source: &Source,
    data: &Payload,
    descriptor: usize,
    mut native: Option<&mut Reader>,
) -> Result<Vec<NativeNode>> {
    let nodes = data
        .array(descriptor, 8, None)?
        .into_iter()
        .map(|at| data.pointer(at))
        .collect::<Result<Vec<_>>>()?;
    if let Some(list) = condition::list(source, data, &nodes, native.as_deref_mut())? {
        return Ok(list);
    }
    nodes
        .into_iter()
        .map(|node| {
            if let Some(translated) = condition::read(source, data, node, native.as_deref_mut())? {
                return Ok(translated);
            }
            let class = data.u32(node - 4)?;
            let kind = data.u8(node + 5)?;
            let size = match (class, kind) {
                (0x80803176, 0) => 8,
                (0x80802DEB, 1) => {
                    ensure!(data.f32(node + 8)? >= 0.0, "Negative source timer");
                    12
                }
                _ => anyhow::bail!("Condition {class:08X} ({kind}) requires runtime translation"),
            };
            let bytes = data
                .0
                .get(node..node + size)
                .context("Truncated source condition")?
                .to_vec();
            Ok(NativeNode { kind, bytes })
        })
        .collect()
}

fn effect(
    source: &Source,
    data: &Payload,
    at: usize,
    native: Option<&mut Reader>,
) -> Result<Action> {
    let class = data.u32(at - 4)?;
    let kind = data.u8(at)?;
    match (class, kind) {
        (0x80802A46, 4) => orb::read(source, data, at),
        (0x808023AA, 31) => record::read(data, at),
        (0x808026E4, 28) => incoming::read(source, data, at, native),
        (0x808027D8, 10) | (0x80802188, 11) => scalar::ammo_modifiers(data, at, kind),
        (0x8080230E, 27) => timer::read(source, data, at, native),
        (0x8080231F, 7) => energy::read(data, at),
        (0x80802482, 12) => weighted::read(data, at),
        (0x808027F1, 13) => scalar::rounds(data, at),
        (0x8080321E, 15) => transfer::read(data, at),
        (0x8080314E, 19) => {
            ensure!(
                data.u8(at + 1)? == 0 && data.u8(at + 2)? <= 1,
                "Unsupported source radar-detail lifecycle or selector"
            );
            // Both callbacks pass +2 on activation and its Boolean inverse on removal.
            // The serialized record is three bytes, without the following alignment.
            Ok(Action::Native {
                node: NativeNode {
                    kind: 20,
                    bytes: vec![20, 0, data.u8(at + 2)?],
                },
            })
        }
        (0x808031C0, 34) => damage::read(source, data, at, native),
        (0x80802AC6, 5) => {
            ensure!(
                data.u8(at + 1)? == 1 && data.u8(at + 2)? <= 3,
                "Unsupported source damage selector"
            );
            Ok(Action::SetDamageType {
                mode: DamageMode::from_byte(data.u8(at + 2)?),
                keep_after_removal: false,
            })
        }
        // An owning-weapon fraction on PS4 keeps the authored refill that Bolts from the Blue
        // was verified with in game. Slot fractions, and the Xbox One build, take the native
        // node that keeps the source's combined rounding.
        (0x8080221F, 14)
            if source.platform() == 7 && data.bytes::<12>(at + 12)? == [0; 12] =>
        {
            ensure!(
                data.u8(at + 1)? == 0 && data.bytes::<2>(at + 6)? == [0; 2],
                "Unknown ammunition flags"
            );
            ensure!(
                (2..=5).all(|i| data.u8(at + i).is_ok_and(|v| v <= 1)),
                "Unknown ammunition selector"
            );
            ensure!(
                data.f32(at + 8)?.is_finite(),
                "Source ammunition fraction is not finite"
            );
            Ok(Action::AddFraction {
                fraction_bits: data.u32(at + 8)?,
                target: AmmunitionTarget::OwningWeapon,
                store: AmmunitionStore::from_byte(data.u8(at + 2)?).context("Ammunition store")?,
                capacity: AmmunitionStore::from_byte(data.u8(at + 4)?)
                    .context("Ammunition capacity")?,
                overflow: data.u8(at + 3)? != 0,
                action_scaled: data.u8(at + 5)? != 0,
            })
        }
        (0x8080221F, 14) => scalar::fraction(data, at),
        (0x80802840, 24) | (0x80802C04, 25) => {
            let size = if kind == 24 { 24 } else { 16 };
            ensure!(
                data.u8(at + 1)? == 1,
                "Unsupported source full-auto lifecycle"
            );
            let mut bytes = data
                .0
                .get(at..at + size)
                .context("Truncated source full-auto effect")?
                .to_vec();
            if kind == 24 {
                ensure!(
                    bytes[4..].iter().all(|v| *v == 0),
                    "Unknown source ability flags"
                );
            } else {
                ensure!(bytes[2..4] == [0, 0], "Unknown source timing flags");
                for offset in [4, 8, 12] {
                    let _ = data.f32(at + offset)?;
                }
            }
            bytes[0] = kind + 4;
            Ok(Action::Native {
                node: NativeNode {
                    kind: kind + 4,
                    bytes,
                },
            })
        }
        (0x80802755, 9) => {
            // The serialized node is 48 bytes. Alignment before a following node
            // can add zero padding, which is absent for a terminal node. Its constant
            // occupies four lanes at +10 rather than a value program. A removal
            // policy of one restores zero when the program ends, as stock Cluster Bombs does
            // for its rocket channel.
            ensure!(
                data.u8(at + 1)? == 1 && data.u32(at + 12)? == 0,
                "Unknown source named property header"
            );
            let key = data.u32(at + 8)?;
            ensure!(
                ![0, u32::MAX, 0x811C_9DC5].contains(&key),
                "Source named property has no key"
            );
            let value_bits = data.u32(at + 16)?;
            ensure!(
                (1..4).all(|lane| data.u32(at + 16 + lane * 4).ok() == Some(value_bits))
                    && f32::from_bits(value_bits).is_finite()
                    && data.bytes::<16>(at + 32)? == [0; 16],
                "Source named property is not one constant"
            );
            Ok(Action::Property {
                key,
                target: data.u8(at + 2)?,
                operation_byte: 0,
                removal: 1,
                value_bits,
                restore_bits: 0,
                ability_mask: data.u32(at + 4)?,
                input: 0,
                flag: data.u8(at + 3)?,
            })
        }
        // Source kind 26 holds native kind 30's retained and operation bytes.
        // Padding before the next object is not part of this three-byte record.
        // Tracking `[1A 01 01]` equals stock Tracking Module `[1E 01 01]`.
        (0x80802B88, 26) => {
            ensure!(data.u8(at + 1)? == 1, "Unknown source weapon count fields");
            Ok(Action::WeaponReferenceCount {
                selector: data.u8(at + 2)?,
            })
        }
        _ => anyhow::bail!("Effect {class:08X} ({kind}) requires runtime translation"),
    }
}

fn program(
    source: &Source,
    tag: u32,
    name: &str,
    native: Option<&mut Reader>,
) -> Result<(Program, usize)> {
    program_inner(source, tag, name, native, &mut Vec::new())
}

fn program_inner(
    source: &Source,
    tag: u32,
    name: &str,
    native: Option<&mut Reader>,
    ancestors: &mut Vec<u32>,
) -> Result<(Program, usize)> {
    ensure!(
        ancestors.len() < 8 && !ancestors.contains(&tag),
        "Source nested perk controllers form a cycle or exceed the depth limit"
    );
    ancestors.push(tag);
    let result = action_program(source, tag, name, native, ancestors);
    ancestors.pop();
    result
}

fn action_program(
    source: &Source,
    tag: u32,
    name: &str,
    mut native: Option<&mut Reader>,
    ancestors: &mut Vec<u32>,
) -> Result<(Program, usize)> {
    ensure!(
        source.reference(tag)? == 0x80802711,
        "Source perk is not an action controller"
    );
    let data = source.read(tag)?;
    ensure!(
        data.u64(0x68)? == 0,
        "Additional source action groups require translation"
    );
    let policy = policy::read(&data, native.as_deref_mut())?;
    let mut omitted_feedback = 0;
    let mut patches = Vec::new();
    let mut damage_profiles = Vec::new();
    let mut actions = Vec::new();
    for at in data
        .array(0x38, 8, None)?
        .into_iter()
        // Both engines activate stored effects from last to first. Program actions
        // are execution ordered, and the native compiler reverses them for storage.
        .rev()
    {
        let at = data.pointer(at)?;
        if matches!(
            (data.u32(at - 4)?, data.u8(at)?),
            (0x80802953, 0) | (0x80802B8D, 1)
        ) {
            let translated = pattern::read(
                source,
                native
                    .as_deref_mut()
                    .context("Pattern translation needs native component layouts")?,
                &data,
                at,
                ancestors,
                &mut omitted_feedback,
            )?;
            for (action, mut patch) in translated {
                patch.action_index = actions.len();
                patches.push(patch);
                actions.push(action);
            }
        } else if data.u32(at - 4)? == 0x808025A9 && data.u8(at)? == 3 {
            let (action, mut profile) = direct::read(
                source,
                native
                    .as_deref_mut()
                    .context("Direct effect translation needs native profiles")?,
                &data,
                at,
            )?;
            profile.action_index = actions.len();
            damage_profiles.push(profile);
            actions.push(action);
        } else {
            actions.push(effect(source, &data, at, native.as_deref_mut())?);
        }
    }
    ensure!(
        !actions.is_empty(),
        "Source choice has no executable effects"
    );
    let mut activation = conditions(source, &data, 0x20, native.as_deref_mut())?.into_iter();
    let mut removal = conditions(source, &data, 0x48, native.as_deref_mut())?.into_iter();
    let mut rearm = conditions(source, &data, 0x58, native.as_deref_mut())?.into_iter();
    let native_trigger = activation.next();
    let trigger = if native_trigger.is_some() {
        Trigger::Native
    } else {
        // Source 6CD410 and native EBCEA0 admit an empty primary activation list
        // on event zero when its compiled mask contains that event. Use the
        // native Always condition without broadening a source routing mask.
        ensure!(
            data.u64(0x80)? == 1,
            "Source empty activation list requires initialization-only routing"
        );
        Trigger::Always
    };
    let mut program = Program {
        name: name.to_owned(),
        trigger,
        duration_ms: 0,
        cooldown_ms: 0,
        native_trigger,
        alternative_triggers: activation.collect(),
        native_removal: removal.next(),
        alternative_removals: removal.collect(),
        native_rearm: rearm.next(),
        alternative_rearms: rearm.collect(),
        actions,
        native_asset_patches: patches,
        damage_profiles,
        policy,
        ..Program::default()
    };
    let (auxiliary, omitted) = feedback::read(source, native, &data, &program)?;
    program.auxiliary = auxiliary;
    omitted_feedback += omitted;
    program.validate_structure().map_err(anyhow::Error::msg)?;
    Ok((program, omitted_feedback))
}

/// Intrinsic plugs (`67FBA961`) set a native weapon's frame, a fixed perk Rise of Iron never had.
/// An exotic's signature perk is the one fixed perk its grid holds, so only that takes the socket.
pub(super) const INTRINSIC_CATEGORY: u64 = 0x67FB_A961;
/// The source item rarity of an exotic. A legendary's first column can also hold one fixed perk,
/// such as Burning Eye's Snapshot or Abyss Defiant's Hive Disruptor, which stays a perk column.
const EXOTIC_RARITY: u64 = 5;
/// The client node kind (`+0x44`) of a grid's infusion node. Grids with one keep an exotic's
/// always-on trait beside it in the first column. Year-one grids have none, hold only damage and
/// Ascend there, and unlock the trait as their last talent.
const INFUSION_NODE: u64 = 1;
/// The source effect class and kind that set a weapon's damage type.
const DAMAGE_EFFECT: (u32, u8) = (0x8080_2AC6, 5);
/// The display-only intrinsic a weapon without a signature perk shows, so the inspect screen
/// still has a frame row. It carries no stats or effects.
const LEGACY_FRAME: &str = "Legacy Frame";
const LEGACY_FRAME_DESCRIPTION: &str = "A relic of an older age, pulled through the timeline.";
/// The Destiny logo, a stock 150-pixel Shadowkeep texture the build fits to a perk glyph. Its icon
/// layer is `80B47ED7`, but a perk icon names the texture.
const LEGACY_FRAME_ICON: &str = "0x80B47ED5";
/// Weapon perk plugs, as opposed to shaders (14), masterworks and trackers (13) and mods (37).
const PERK_ITEM_KIND: u64 = 15;
/// Shader plugs.
pub(super) const SHADER_ITEM_KIND: u64 = 14;
/// Shadowkeep's Default Shader plug (4248210736). A donor's shader socket can default to one of
/// its own shader presets instead, such as 4F98F7A7 on Twilight Garrison's donor.
const DEFAULT_SHADER: u64 = 0xFD36_8D30;
/// Year-one armor trait mods: Plasteel Reinforcement and Restorative Mod. They are Shadowkeep's
/// own choice, not a Rise of Iron talent, so an armor import disables the socket.
const ARMOR_TRAIT_CATEGORY: u64 = 0xC57B_725E;
/// Year-one weapon damage mod sockets, which Shadowkeep labels Weapon Mod: 68 offers an energy
/// weapon's Arc, Solar or Void Damage Mod and 69 a kinetic weapon's Kinetic Damage Mod. Rise of
/// Iron weapons have no mods and the imported weapon carries its damage type itself, so the
/// socket holds a talent column the base's perk sockets cannot, or else is disabled.
const DAMAGE_MOD_SOCKET_TYPES: [u64; 2] = [68, 69];

/// Weapon properties whose only gameplay consumer is another weapon's firing graph, and the
/// behavior that grafts it. Zhalo Supercell's chain lightning writes `chain_enabled`, which
/// Riskrunner's firing graph 80BC10AF reads in its projectile controller 8152C79F. Riskrunner's
/// own Superconductor writes it only while Arc Conductor holds `perk_active`, but the source
/// program writes it on its own, with `perk_active` beside it (see `PROPERTY_COMPANIONS`), so
/// the graft leaves both Riskrunner perks out.
const PROPERTY_BEHAVIORS: [(u32, &str); 1] = [(0x65D8_F47E, "riskrunner-graph")];

/// Weapon properties a grafted firing graph reads together, and the one a source program writes
/// alone. Riskrunner's projectile controller 8152C79F and its effect graph 8152C7A1 read
/// `perk_active` (A43A8C2E) beside `chain_enabled`: Arc Conductor (perk 493, action 80BBCA57)
/// holds `perk_active` at 1 while it runs, and Superconductor (495) writes `chain_enabled` only
/// then. Zhalo Supercell's source proc chains on its own, so the same proc holds both, written
/// the way Arc Conductor writes it, without bringing Arc Conductor's damage resistance or ammo.
const PROPERTY_COMPANIONS: [(u32, u32); 1] = [(0x65D8_F47E, 0xA43A_8C2E)];

/// Add the companion write after each weapon property write that needs one.
fn companions(program: &mut Program) {
    let mut index = 0;
    while index < program.actions.len() {
        if let Action::Property { key, target: 0, .. } = program.actions[index]
            && let Some(&(_, companion)) = PROPERTY_COMPANIONS.iter().find(|(k, _)| *k == key)
            && !program.actions.iter().any(
                |action| matches!(action, Action::Property { key, target: 0, .. } if *key == companion),
            )
        {
            let mut write = program.actions[index].clone();
            if let Action::Property { key, .. } = &mut write {
                *key = companion;
            }
            index += 1;
            program.actions.insert(index, write);
        }
        index += 1;
    }
}

/// The behavior graphs the translated weapon programs need, by catalogue identifier.
fn property_behaviors(programs: &[Program]) -> Vec<&'static str> {
    let mut behaviors = Vec::new();
    for program in programs {
        for action in &program.actions {
            if let Action::Property { key, target: 0, .. } = action
                && let Some(&(_, behavior)) = PROPERTY_BEHAVIORS.iter().find(|(k, _)| k == key)
                && !behaviors.contains(&behavior)
            {
                behaviors.push(behavior);
            }
        }
    }
    behaviors
}

fn damage_name(mode: DamageMode) -> Option<&'static str> {
    match mode {
        DamageMode::Kinetic => Some("kinetic"),
        DamageMode::Solar => Some("solar"),
        DamageMode::Arc => Some("arc"),
        DamageMode::Void => Some("void"),
        DamageMode::Unknown(_) => None,
    }
}

/// The damage type a translated source choice sets, from the first of its programs that sets one.
fn damage_type(programs: &[Program]) -> Option<&'static str> {
    programs
        .iter()
        .flat_map(|program| &program.actions)
        .find_map(|action| match action {
            Action::SetDamageType { mode, .. } => damage_name(*mode),
            _ => None,
        })
}

/// Whether setting the damage type is all a translated choice does, so the recipe's damage type
/// can stand in for it.
fn only_sets_damage(lowered: &Lowered) -> bool {
    matches!(lowered.raw.as_slice(), [program] if program.actions.len() == 1)
        && lowered.stats.is_empty()
        && lowered.stock.is_empty()
        && lowered.grants.is_empty()
}

/// Whether `program` acts for as long as its item is equipped: an Always program with no other
/// activation, ending, chance, duration or complete native form. Rise of Iron programs lower to
/// Always or to a native event trigger.
fn unconditional(program: &Program) -> bool {
    program.trigger == Trigger::Always
        && program.native_trigger.is_none()
        && program.alternative_triggers.is_empty()
        && program.native_removal.is_none()
        && program.alternative_removals.is_empty()
        && program.removal_key.is_none()
        && program.native.is_none()
        && program.chance_permyriad == 10_000
        && program.duration_ms == 0
}

/// Remove the element changes from `lowered`'s unconditional programs, since the weapon's damage
/// type stands in for them, and return whether any was removed. Later actions' indices shift down.
/// A program left with no action goes with its finished perk. An element change an event starts
/// stays in its program.
fn strip_elements(lowered: &mut Lowered) -> Result<bool> {
    let mut stripped = false;
    let mut index = 0;
    while index < lowered.raw.len() {
        let program = &mut lowered.raw[index];
        let removed = program
            .actions
            .iter()
            .enumerate()
            .filter(|(_, action)| {
                matches!(action, Action::SetDamageType { mode, .. } if damage_name(*mode).is_some())
            })
            .map(|(at, _)| at)
            .collect::<Vec<_>>();
        if removed.is_empty() || !unconditional(program) {
            index += 1;
            continue;
        }
        for &at in removed.iter().rev() {
            program.actions.remove(at);
        }
        let shift = |at: &mut usize| -> Result<()> {
            ensure!(
                !removed.contains(at),
                "An element change carries a native asset edit"
            );
            *at -= removed.iter().filter(|&&r| r < *at).count();
            Ok(())
        };
        for patch in &mut program.native_asset_patches {
            shift(&mut patch.action_index)?;
        }
        for profile in &mut program.damage_profiles {
            shift(&mut profile.action_index)?;
        }
        for asset in &mut program.imported_assets {
            shift(&mut asset.action_index)?;
        }
        stripped = true;
        if program.actions.is_empty() {
            ensure!(
                program.native_asset_patches.is_empty()
                    && program.damage_profiles.is_empty()
                    && program.imported_assets.is_empty(),
                "A program of only element changes carries native assets"
            );
            lowered.raw.remove(index);
            lowered.programs.remove(index);
            continue;
        }
        lowered.programs[index]["program"] = serde_json::to_value(&lowered.raw[index])?;
        index += 1;
    }
    Ok(stripped)
}

/// Every source step whose talent actions carry a damage effect, as its node, step and damage
/// type. The effects are read from the source actions, so a step counts even when its other
/// contributions keep it from translating. A selector outside the four types reads as `None`.
/// Only each action's own effect list is read, not nested controllers or additional groups.
fn source_damage(
    source: &Source,
    nodes: &[Value],
) -> Result<Vec<(u64, u64, Option<&'static str>)>> {
    let mut found = Vec::new();
    for node in nodes {
        let index = node["index"].as_u64().context("Source talent node index")?;
        for step in node["steps"].as_array().context("Source talent steps")? {
            let ordinal = step["index"].as_u64().context("Source talent step index")?;
            let perks = step["variants"]
                .as_array()
                .context("Source talent variants")?
                .iter()
                .flat_map(|variant| variant["perks"].as_array().into_iter().flatten());
            for perk in perks {
                let Some(tag) = perk["action"]
                    .as_u64()
                    .and_then(|tag| u32::try_from(tag).ok())
                else {
                    continue;
                };
                if source.reference(tag)? != 0x8080_2711 {
                    continue;
                }
                let data = source.read(tag)?;
                for at in data.array(0x38, 8, None)? {
                    let at = data.pointer(at)?;
                    if (data.u32(at - 4)?, data.u8(at)?) == DAMAGE_EFFECT {
                        let kind = if data.u8(at + 1)? == 1 {
                            damage_name(DamageMode::from_byte(data.u8(at + 2)?))
                        } else {
                            None
                        };
                        found.push((index, ordinal, kind));
                    }
                }
            }
        }
    }
    Ok(found)
}

/// The damage type the source effects give a weapon: the one type all of them select, or Kinetic
/// when no talent carries one, since Rise of Iron applies an element only through such an effect.
/// A mix of types or an unreadable selector gives none.
fn effect_damage(found: &[(u64, u64, Option<&'static str>)]) -> Option<&'static str> {
    let kinds = found
        .iter()
        .map(|&(_, _, kind)| kind)
        .collect::<BTreeSet<_>>();
    match kinds.into_iter().collect::<Vec<_>>().as_slice() {
        [] => Some("kinetic"),
        [kind] => *kind,
        _ => None,
    }
}

/// A source weapon whose effects make it Kinetic needs a donor that can carry Kinetic damage.
/// Damage alternatives mix types, so a weapon with selectable damage keeps any donor. Unsupported
/// choices count, since the weapon's damage does not depend on their translation.
pub(super) fn requires_kinetic(source: &Source, weapon: &Value) -> Result<bool> {
    let nodes = weapon["talents"]["nodes"]
        .as_array()
        .context("Source talent nodes")?;
    Ok(effect_damage(&source_damage(source, nodes)?) == Some("kinetic"))
}

/// A native perk socket: its index, socket type and perk plugs, a default first when present.
struct PerkSocket<'a> {
    index: usize,
    socket_type: &'a Value,
    plugs: Vec<u32>,
}

/// The base's intrinsic socket and its other perk sockets, in socket order.
fn perk_sockets(
    sockets: &[Value],
    host: Host,
) -> Result<(Option<PerkSocket<'_>>, Vec<PerkSocket<'_>>)> {
    let mut intrinsic = None;
    let mut perks = Vec::new();
    for (index, socket) in sockets.iter().enumerate() {
        if socket["socket_type"] == u16::MAX {
            continue;
        }
        let Some(choices) = socket["choices"].as_array() else {
            continue;
        };
        // An armor base's intrinsic is a stat package rather than a perk plug. Only an exotic's
        // signature talent takes it, see `translate`, so its stat plugs serve as the layout.
        if host == Host::Armor
            && intrinsic.is_none()
            && !choices.is_empty()
            && choices
                .iter()
                .all(|choice| choice["category"].as_u64() == Some(INTRINSIC_CATEGORY))
        {
            let mut plugs = Vec::new();
            for choice in choices
                .iter()
                .filter(|choice| choice["hash"] == socket["curated_fallback"])
                .chain(choices)
            {
                let hash = u32::try_from(choice["hash"].as_u64().context("Native plug identity")?)?;
                if !plugs.contains(&hash) {
                    plugs.push(hash);
                }
            }
            intrinsic = Some(PerkSocket {
                index,
                socket_type: &socket["socket_type"],
                plugs,
            });
            continue;
        }
        // Randomized trait sockets can omit a default entirely. Their homogeneous
        // perk pool still supplies a socket layout for private source choices.
        // Mixed or cosmetic pools cannot establish that role without a default.
        let default = choices.iter().find(|choice| choice["default"] == true);
        let fallback = || {
            choices
                .iter()
                .all(|choice| choice["item_kind"].as_u64() == Some(PERK_ITEM_KIND))
                .then(|| {
                    choices
                        .iter()
                        .find(|choice| choice["hash"] == socket["curated_fallback"])
                        .or_else(|| choices.first())
                })
                .flatten()
        };
        let Some(default) = default.or_else(fallback) else {
            continue;
        };
        if default["item_kind"].as_u64() != Some(PERK_ITEM_KIND) {
            continue;
        }
        let mut plugs = Vec::new();
        for choice in std::iter::once(default).chain(choices.iter().filter(|choice| {
            choice["default"] != true && choice["item_kind"].as_u64() == Some(PERK_ITEM_KIND)
        })) {
            let hash = u32::try_from(choice["hash"].as_u64().context("Native plug identity")?)?;
            if !plugs.contains(&hash) {
                plugs.push(hash);
            }
        }
        let socket = PerkSocket {
            index,
            socket_type: &socket["socket_type"],
            plugs,
        };
        if host == Host::Armor || default["category"].as_u64() != Some(INTRINSIC_CATEGORY) {
            perks.push(socket);
        } else if intrinsic.is_none() {
            intrinsic = Some(socket);
        }
    }
    Ok((intrinsic, perks))
}

/// A translated source choice and where it sits in the source grid.
struct Choice {
    column: i64,
    row: i64,
    /// The source node, one of the column's exclusive rows. Rise of Iron rolled one of its steps.
    node: u64,
    name: String,
    description: Value,
    icon: Option<String>,
    lowered: Lowered,
    report: Value,
    /// The element the choice sets, which the weapon's damage type takes instead of a socket.
    element: Option<&'static str>,
}

/// The finished perks that carry translated programs, and the plug that lent the first. A donor
/// choice supplies only a finished-perk layout. Empty sight and magazine plugs are valid
/// choices, so search the remaining default and alternate choices before refusing it. A private
/// plug clones each finished perk once, so a talent step with several source perks needs a
/// different one for each, and the search continues until it has `needed` of them.
fn templates(
    assembly: &mut Assembly<'_>,
    sockets: &[Value],
    needed: usize,
) -> Result<(u32, Vec<u16>)> {
    let mut visited = BTreeSet::new();
    let mut lender = None;
    let mut templates = Vec::new();
    for default in [true, false] {
        for choice in sockets
            .iter()
            .flat_map(|socket| socket["choices"].as_array().into_iter().flatten())
        {
            if templates.len() >= needed {
                break;
            }
            if choice["item_kind"].as_u64() != Some(PERK_ITEM_KIND)
                || (choice["default"] == true) != default
            {
                continue;
            }
            let hash = u32::try_from(choice["hash"].as_u64().context("Native plug identity")?)?;
            if !visited.insert(hash) {
                continue;
            }
            let tag = crate::tiger::shadowkeep::item_tag(&mut assembly.native, hash)?;
            let item = assembly.native.tag(tag, Some(0x80807BEA))?;
            let perks = sundial::package_authoring::native_weapon::base_sandbox_perks(&item.0)
                .map_err(anyhow::Error::msg)?;
            for perk in perks {
                if templates.len() < needed && !templates.contains(&perk) {
                    lender.get_or_insert(hash);
                    templates.push(perk);
                }
            }
        }
    }
    let lender = lender.context("Native gameplay plug choices have no finished perk template")?;
    Ok((lender, templates))
}

/// The most choices a perk column offers freely, as few as a Shadowkeep random-roll column's
/// curated perks.
const REUSABLE_CHOICES: usize = 4;
/// The most choices a column rolls from: the rows one 64-bit ownership mask covers, also the
/// largest stock randomized set.
const ROLL_POOL: usize = 64;

/// Source choices in row and node order, retaining step order within each node.
fn members(choices: &[Choice], column: i64) -> Vec<usize> {
    let mut members = choices
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| (choice.column == column).then_some(index))
        .collect::<Vec<_>>();
    members.sort_by_key(|&index| (choices[index].row, choices[index].node));
    members
}

/// A column's choices in socket order, and how many lead it as freely swappable. A source
/// column's nodes are exclusive rows, and Rise of Iron rolled one step of each. The first
/// supported step of each of the first four nodes stays swappable. Every supported step joins the
/// roll pool, the swappable ones first and then the rest in row and step order.
fn roll(
    choices: &[Choice],
    members: Vec<usize>,
    unsupported: &mut Vec<Value>,
) -> (Vec<usize>, usize) {
    let mut nodes = BTreeSet::new();
    let (mut ordered, rolled): (Vec<_>, Vec<_>) = members
        .into_iter()
        .partition(|&index| nodes.insert(choices[index].node) && nodes.len() <= REUSABLE_CHOICES);
    let reusable = ordered.len();
    for index in rolled {
        if ordered.len() < ROLL_POOL {
            ordered.push(index);
        } else {
            let choice = &choices[index];
            unsupported.push(json!({"node":choice.node,"step":choice.report["step"],
                "name":choice.name,"reason":"The column's roll pool is full"}));
        }
    }
    (ordered, reusable)
}

/// A column's choices without repeats. Rise of Iron's nodes can each roll the same talent, as
/// Twilight Garrison's two Increase nodes both hold Increase Discipline, and a column that offers
/// every step keeps one choice for each distinct talent.
fn distinct(choices: &[Choice], members: Vec<usize>, merged: &mut Vec<Value>) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::new();
    for index in members {
        let choice = &choices[index];
        let same = |other: &Choice| {
            other.name == choice.name
                && other.description == choice.description
                && other.lowered.programs == choice.lowered.programs
                && other.lowered.stats == choice.lowered.stats
                && other.lowered.stock == choice.lowered.stock
        };
        if let Some(&first) = kept.iter().find(|&&other| same(&choices[other])) {
            let first = &choices[first];
            merged.push(
                json!({"node":choice.node,"step":choice.report["step"],"name":choice.name,
                "same_as":{"node":first.node,"step":first.report["step"]}}),
            );
        } else {
            kept.push(index);
        }
    }
    kept
}

/// Whether a source node carries a talent: perks, stat rows or ability grants. Infusion, Ascend,
/// damage upgrade and ornament nodes carry none.
fn carries_talent(node: &Value) -> bool {
    node["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|step| step["variants"].as_array().into_iter().flatten())
        .any(|variant| {
            ["perks", "stat_adjustments", "ability_grants"]
                .into_iter()
                .any(|field| {
                    variant[field]
                        .as_array()
                        .is_some_and(|rows| !rows.is_empty())
                })
        })
}

/// The source column of an exotic's trait, decided by the grid's structure rather than by which
/// talents translate. A grid with an infusion node keeps the trait as its first column's one talent
/// node. Otherwise, or when the first column holds no single talent, the trait is the talent of the
/// last column that holds any, the grid's final unlock. Nodes that set the damage type are never
/// the trait, and the trait's column holds one talent node with one step.
fn trait_column(nodes: &[Value], damage_nodes: &BTreeSet<u64>) -> Result<Option<i64>> {
    let mut talents = Vec::new();
    for node in nodes {
        let index = node["index"].as_u64().context("Source talent node index")?;
        if carries_talent(node) && !damage_nodes.contains(&index) {
            let column = node["column"].as_i64().context("Source talent column")?;
            talents.push((column, node["steps"].as_array().map_or(0, Vec::len)));
        }
    }
    let single = |column: i64| {
        let mut members = talents.iter().filter(|&&(at, _)| at == column);
        matches!((members.next(), members.next()), (Some((_, 1)), None))
    };
    let infused = nodes
        .iter()
        .any(|node| node["column"] == 0 && node["client_kind"] == INFUSION_NODE);
    if infused && single(0) {
        return Ok(Some(0));
    }
    Ok(talents
        .iter()
        .map(|&(column, _)| column)
        .filter(|&column| column >= 0)
        .max()
        .filter(|&column| single(column)))
}

/// Translate the source talent grid onto the base's perk sockets. On an exotic, the signature
/// perk takes the intrinsic socket: a single-step node alone in its column and named after the
/// item, or else a first source column that holds one single-step node, or else the trait that
/// [`trait_column`] finds. Otherwise the intrinsic holds Legacy Frame, since a native frame would
/// describe a perk the source never had.
/// Each other source column becomes one socket, taking the base's perk sockets after its
/// intrinsic in order, so the sights column lands where a native weapon's barrel or sight does.
/// A column offers one step of each of its first four nodes, in row order, and rolls from every
/// supported step, as a Shadowkeep random-roll column keeps a few curated perks beside its
/// randomized pool. Each choice clones a plug from
/// its native socket's pool as its layout template, distinct while the pool lasts. A choice whose
/// only effect sets a fixed damage type sets the recipe's damage type. Alternative damage steps
/// would be damage mods, so only the first is kept, as the weapon's damage type. A choice that
/// sets the element and does more keeps the rest in its own column, and the damage type takes the
/// element. Without a translated element, the source effects set the recipe's damage type, Kinetic
/// when no talent sets an element.
/// Unused perk sockets are disabled so the base's own perks do not stand in for the source's.
/// The base's damage mod sockets hold columns past its perk sockets before any socket is added,
/// and are otherwise disabled, since an imported weapon offers no damage mods.
#[allow(clippy::cognitive_complexity)]
pub(super) fn translate(
    assembly: &mut Assembly<'_>,
    source: &Value,
    donor: u32,
    host: Host,
) -> Result<(Value, Value, Value, Option<&'static str>)> {
    let native = crate::tiger::shadowkeep::sockets(&mut assembly.native, donor)?;
    let definitions = crate::tiger::shadowkeep::stat_definitions(&mut assembly.native)?;
    let sockets = native["sockets"].as_array().context("Native sockets")?;
    let nodes = source["talents"]["nodes"]
        .as_array()
        .context("Source talent nodes")?;
    // A talent step carries each of its source perks on a finished perk of its own.
    let needed = nodes
        .iter()
        .flat_map(|node| node["steps"].as_array().into_iter().flatten())
        .flat_map(|step| step["variants"].as_array().into_iter().flatten())
        .map(|variant| {
            variant["perks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|perk| perk["action"].as_u64().is_some())
                .count()
                + usize::from(
                    variant["ability_grants"]
                        .as_array()
                        .is_some_and(|g| !g.is_empty()),
                )
        })
        .max()
        .unwrap_or_default()
        .max(1);
    let (intrinsic, perk_sockets) = perk_sockets(sockets, host)?;
    // Armor has no damage type, so only a weapon's damage effects are read.
    let found = if host == Host::Weapon {
        source_damage(assembly.source, nodes)?
    } else {
        Vec::new()
    };
    let armor_lender = if host == Host::Armor && perk_sockets.is_empty() {
        Some(armor::layout(&mut assembly.native, donor)?)
    } else {
        None
    };
    let template_sockets = armor_lender
        .as_ref()
        .map(|lender| {
            lender["sockets"]
                .as_array()
                .context("Armor perk layout sockets")
        })
        .transpose()?
        .unwrap_or(sockets);
    let (_, extra_layouts) = self::perk_sockets(template_sockets, host)?;
    let (template_hash, templates) = templates(assembly, template_sockets, needed)?;
    let mut equivalents = BTreeMap::new();
    for (_, source, plug) in STOCK_EQUIVALENTS
        .into_iter()
        .filter(|(kind, ..)| *kind == host)
    {
        let tag = crate::tiger::shadowkeep::item_tag(&mut assembly.native, plug)
            .with_context(|| format!("Stock perk plug {plug:08X}"))?;
        let data = assembly.native.tag(tag, Some(0x80807BEA))?;
        let perks = sundial::package_authoring::native_weapon::base_sandbox_perks(&data.0)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            !perks.is_empty(),
            "Stock perk plug {plug:08X} has no sandbox perk"
        );
        equivalents.insert(source, (plug, perks));
    }
    let mut columns = sockets.iter().map(|_| Value::Null).collect::<Vec<_>>();
    // Rise of Iron items offer only the Default Shader, not the donor's own shader list.
    for (index, socket) in sockets.iter().enumerate() {
        let shader = socket["choices"].as_array().is_some_and(|choices| {
            choices
                .iter()
                .any(|c| c["default"] == true && c["item_kind"].as_u64() == Some(SHADER_ITEM_KIND))
        });
        if shader {
            columns[index] = json!({
                "choices":[format!("0x{DEFAULT_SHADER:08X}")],
                "socket_type":socket["socket_type"]
            });
        }
    }
    // An armor intrinsic keeps its stat package unless the signature talent takes it.
    for socket in intrinsic
        .iter()
        .filter(|_| host == Host::Weapon)
        .chain(&perk_sockets)
    {
        columns[socket.index] = json!({"choices":[],"socket_type":u16::MAX});
    }
    if host == Host::Armor {
        for (index, socket) in sockets.iter().enumerate() {
            if socket["choices"].as_array().is_some_and(|choices| {
                !choices.is_empty()
                    && choices
                        .iter()
                        .all(|choice| choice["category"].as_u64() == Some(ARMOR_TRAIT_CATEGORY))
            }) {
                columns[index] = json!({"choices":[],"socket_type":u16::MAX});
            }
        }
    }
    // Damage mod sockets, in socket order, for talent columns past the base's perk sockets.
    let mut mod_sockets = sockets
        .iter()
        .enumerate()
        .filter(|(_, socket)| {
            host == Host::Weapon
                && socket["socket_type"]
                    .as_u64()
                    .is_some_and(|kind| DAMAGE_MOD_SOCKET_TYPES.contains(&kind))
        })
        .map(|(index, _)| index)
        .collect::<std::collections::VecDeque<_>>();
    for &index in &mod_sockets {
        columns[index] = json!({"choices":[],"socket_type":u16::MAX});
    }
    let mut choices = Vec::new();
    let mut unsupported = Vec::new();
    let mut merged = Vec::new();
    let mut grant_catalog = None;
    for node in nodes {
        let column = node["column"].as_i64().context("Source talent column")?;
        let row = node["row"].as_i64().context("Source talent row")?;
        let node_index = node["index"].as_u64().context("Source talent node index")?;
        for step in node["steps"].as_array().context("Source talent steps")? {
            let name = step["name"].as_str().context("Source talent name")?;
            let lowered = (|| -> Result<Lowered> {
                let variants = step["variants"]
                    .as_array()
                    .context("Source talent variants")?;
                ensure!(
                    variants.len() == 1,
                    "Talent step has multiple runtime variants"
                );
                let record = Payload(hex::decode(
                    variants[0]["record"]
                        .as_str()
                        .context("Source variant bytes")?,
                )?);
                host.validate_variant(step, &record)?;
                let (stats, adjusted) = adjustments(
                    host,
                    &record,
                    &variants[0]["stat_adjustments"],
                    &definitions,
                )?;
                let perks = variants[0]["perks"]
                    .as_array()
                    .context("Source talent perks")?;
                let grants = variants[0]["ability_grants"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                ensure!(
                    grants.len() == usize::try_from(record.u64(0)?)?,
                    "Source ability grants disagree with the variant record"
                );
                // A choice with only stat rows becomes a stats-only private plug.
                ensure!(
                    !perks.is_empty() || !stats.is_empty() || !grants.is_empty(),
                    "Talent step has no standalone runtime perk"
                );
                let mut programs = Vec::new();
                let mut raw = Vec::new();
                let mut tags = Vec::new();
                let mut auxiliary = 0;
                let mut stock = Vec::new();
                let mut stock_plugs = Vec::new();
                for perk in perks {
                    let Some(tag) = perk["action"].as_u64() else {
                        // A grant-backed talent can have a display perk without its own action.
                        // The complete grant array must lower successfully below.
                        ensure!(
                            !grants.is_empty() && perk["runtime_key"].as_u64() == Some(0x811C9DC5),
                            "Source perk has no resolved standalone action"
                        );
                        continue;
                    };
                    let tag = u32::try_from(tag)?;
                    tags.push(tag);
                    let identity =
                        u32::try_from(perk["hash"].as_u64().context("Source perk identity")?)?;
                    // A stock equivalent replaces the source action, which is not lowered.
                    if let Some((plug, indices)) = equivalents.get(&identity) {
                        stock.extend(indices.iter().copied());
                        stock_plugs.push(format!("{plug:08X}"));
                        continue;
                    }
                    host.validate_action(assembly.source, tag)?;
                    let (mut program, omitted) =
                        program(assembly.source, tag, name, Some(&mut assembly.native))?;
                    if host == Host::Weapon {
                        companions(&mut program);
                    }
                    auxiliary += omitted;
                    let carrier = templates.get(programs.len()).context(
                        "The base has no further finished perk to carry this source perk",
                    )?;
                    programs.push(json!({"source_perk_index":carrier,"program":program,"runtime_values":[],"action_float_values":[]}));
                    raw.push(program);
                }
                let mut granted = Vec::new();
                if !grants.is_empty() {
                    if grant_catalog.is_none() {
                        grant_catalog =
                            Some(grant::Catalog::read(assembly.source, &mut assembly.native)?);
                    }
                    let (program, report) = grant_catalog
                        .as_ref()
                        .context("Source ability catalog")?
                        .lower(assembly.source, &mut assembly.native, &variants[0], name)?;
                    granted = report;
                    if !program.actions.is_empty() {
                        let carrier = templates.get(programs.len()).context(
                            "The base has no further finished perk for source ability properties",
                        )?;
                        programs.push(json!({"source_perk_index":carrier,"program":program,"runtime_values":[],"action_float_values":[]}));
                        raw.push(program);
                    }
                }
                ensure!(
                    !programs.is_empty() || !stats.is_empty() || !stock.is_empty(),
                    "Talent step has no executable action, property or stat contribution"
                );
                let audio_cues = audio::program_cues(&raw)?;
                Ok(Lowered {
                    programs,
                    raw,
                    tags,
                    auxiliary,
                    stats,
                    adjustments: adjusted,
                    stock,
                    stock_plugs,
                    grants: granted,
                    audio_cues,
                })
            })();
            let mut lowered = match lowered {
                Ok(lowered) => lowered,
                Err(error) => {
                    unsupported.push(json!({"node":node["index"],"step":step["index"],"name":name,"reason":format!("{error:#}")}));
                    continue;
                }
            };
            let mut report = json!({"node":node["index"],"step":step["index"],"name":name,
                "column":column,"row":row,"source_actions":lowered.tags,
                "stat_adjustments":lowered.adjustments,"stock_perk_plugs":lowered.stock_plugs,
                "ability_grants":lowered.grants,
                "source_audio_cues":lowered.audio_cues,
                "omitted_feedback_records":lowered.auxiliary});
            let container = u32::try_from(step["icon"].as_u64().unwrap_or(0))?;
            let icon = assembly
                .talent_icon(container)
                .with_context(|| format!("Source talent {name} icon"))?;
            // Rise of Iron set elements through talents, which Shadowkeep gives the weapon's damage
            // type instead, so no socket or plug offers an element. A choice that only sets the
            // element leaves its perk column. One that does more keeps the rest in its column,
            // without the element change when that lasts while the weapon is equipped. One an
            // event starts keeps its element change, and the damage type still takes the element.
            let element = damage_type(&lowered.raw);
            let column = if element.is_none() {
                column
            } else if only_sets_damage(&lowered) {
                i64::MIN
            } else {
                let stripped = strip_elements(&mut lowered)?;
                report["element_assigned_to_damage_type"] = json!(element);
                report["element_action_removed"] = json!(stripped);
                if lowered.programs.is_empty()
                    && lowered.stats.is_empty()
                    && lowered.stock.is_empty()
                    && lowered.grants.is_empty()
                {
                    i64::MIN
                } else {
                    column
                }
            };
            choices.push(Choice {
                column,
                row,
                node: node_index,
                name: name.to_owned(),
                description: step["description"].clone(),
                icon,
                lowered,
                report,
                element,
            });
        }
    }
    audio::admit(assembly, &mut choices, &mut unsupported)?;
    let damage_kinds = choices
        .iter()
        .filter(|c| c.column == i64::MIN)
        .filter_map(|c| damage_type(&c.lowered.raw))
        .collect::<BTreeSet<_>>();
    if damage_kinds.len() > 1 {
        let damage_nodes = choices
            .iter()
            .filter(|c| c.column == i64::MIN)
            .map(|c| c.report["node"].as_u64().context("Damage talent node"))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(
            damage_nodes.len() == 1,
            "Damage alternatives belong to different source nodes"
        );
        // Alternative damage steps would be damage mods, which an imported weapon does not
        // offer. It keeps the first source step's damage type, which the recipe can change.
        let first = choices.iter().find_map(|c| c.element);
        choices.retain(|choice| {
            let kind = (choice.column == i64::MIN)
                .then(|| damage_type(&choice.lowered.raw))
                .flatten();
            if kind.is_none() || kind == first {
                return true;
            }
            unsupported.push(json!({"node":choice.node,"step":choice.report["step"],
                "name":choice.name,"reason":"Alternative damage types are damage mods, which an imported weapon does not offer. The weapon keeps the first source damage type."}));
            false
        });
    }
    for choice in choices.iter_mut().filter(|c| c.column == i64::MAX) {
        choice.report["damage_choice"] = json!(damage_type(&choice.lowered.raw));
    }
    // The first element a choice sets, in source order, gives the weapon its damage type. Without
    // one, the source effects decide it.
    let chosen = choices.iter().find_map(|c| c.element);
    let damage = chosen.or_else(|| {
        (host == Host::Weapon)
            .then(|| effect_damage(&found))
            .flatten()
    });
    let damage_origin = match (chosen, damage) {
        (Some(_), _) => Some("talent"),
        (None, Some(_)) if found.is_empty() => Some("no_source_element"),
        (None, Some(_)) => Some("source_effect"),
        (None, None) => None,
    };
    let mut source_columns = choices
        .iter()
        .map(|choice| choice.column)
        .filter(|&column| column != i64::MIN)
        .collect::<Vec<_>>();
    source_columns.sort_unstable();
    source_columns.dedup();
    let mut variants = Vec::new();
    let mut translated = Vec::new();
    for choice in choices.iter().filter(|choice| choice.column == i64::MIN) {
        let mut report = choice.report.clone();
        report["damage_type"] = json!(damage);
        translated.push(report);
    }
    // A node with one step is fixed in the source grid, not a roll or a choice.
    let mut steps = BTreeMap::new();
    for node in nodes {
        let index = node["index"].as_u64().context("Source talent node")?;
        steps.insert(index, node["steps"].as_array().map_or(0, Vec::len));
    }
    // A column whose only choice is one fixed step: the shape of an exotic's signature perk.
    let fixed = |column: i64| -> Option<&Choice> {
        let members = choices
            .iter()
            .filter(|choice| choice.column == column)
            .collect::<Vec<_>>();
        (members.len() == 1
            && members[0].report["node"]
                .as_u64()
                .and_then(|node| steps.get(&node))
                == Some(&1))
        .then(|| members[0])
    };
    let exotic = intrinsic.is_some() && source["item"]["rarity"].as_u64() == Some(EXOTIC_RARITY);
    let item_name = source["item"]["name"]
        .as_str()
        .map(|name| name.trim().to_lowercase());
    let damage_nodes = found
        .iter()
        .map(|&(node, _, _)| node)
        .collect::<BTreeSet<_>>();
    let trait_at = if exotic {
        trait_column(nodes, &damage_nodes)?
    } else {
        None
    };
    let signature = exotic
        .then(|| {
            // A fixed perk named after the item is its signature wherever its column sits, as
            // The First Curse's is in column 4 behind fixed Dead Eye and Triple Tap. Rise of
            // Iron marks neither by node flags: Dead Eye and Empyrean Bellicose's Antigrav
            // Thrusters both start unlocked outside an exclusive set, while The First Curse's
            // perk unlocks later inside its own set. The grid's structural trait comes last,
            // and one that does not translate leaves Legacy Frame in the intrinsic socket.
            source_columns
                .iter()
                .copied()
                .find(|&column| {
                    fixed(column).is_some_and(|choice| {
                        item_name.as_deref() == Some(choice.name.trim().to_lowercase().as_str())
                    })
                })
                .or_else(|| {
                    source_columns
                        .first()
                        .copied()
                        .filter(|&column| column == 0 && fixed(column).is_some())
                })
                .or_else(|| trait_at.filter(|&column| fixed(column).is_some()))
        })
        .flatten();
    let mut ordinal = 0;
    for &column in &source_columns {
        let members = members(&choices, column);
        // Armor offers every distinct step of each talent, as an owned piece can take any plug of
        // its column's pool. Damage alternatives are steps of one node, which a roll would cut to
        // its first step, so every one stays selectable instead.
        let (members, reusable) = if host == Host::Armor || column == i64::MAX {
            let mut members = if host == Host::Armor {
                distinct(&choices, members, &mut merged)
            } else {
                members
            };
            for index in members.drain(ROLL_POOL.min(members.len())..) {
                let choice = &choices[index];
                unsupported.push(json!({"node":choice.node,"step":choice.report["step"],
                    "name":choice.name,"reason":"The column's roll pool is full"}));
            }
            let count = members.len();
            (members, count)
        } else {
            roll(&choices, members, &mut unsupported)
        };
        // The runtime base supplies socket layouts, not a limit on source talent columns.
        // Additional columns use a normal perk layout and still receive private plug identities,
        // in the base's damage mod socket first so they keep the base's socket order.
        let extra;
        let socket = if signature == Some(column)
            && let Some(socket) = &intrinsic
        {
            socket
        } else if let Some(socket) = perk_sockets.get(ordinal) {
            ordinal += 1;
            socket
        } else {
            let layout = perk_sockets
                .last()
                .or_else(|| extra_layouts.last())
                .context("Native base has no perk socket layout")?;
            let index = mod_sockets.pop_front().unwrap_or_else(|| {
                columns.push(Value::Null);
                columns.len() - 1
            });
            extra = PerkSocket {
                index,
                socket_type: layout.socket_type,
                plugs: layout.plugs.clone(),
            };
            &extra
        };
        let mut plugs = Vec::new();
        for (choice_index, index) in members.into_iter().enumerate() {
            let choice = &choices[index];
            // The template only lends its layout. Each choice still becomes its own private item,
            // so a pool shorter than the column repeats its plugs.
            let plug = socket.plugs[choice_index % socket.plugs.len()];
            plugs.push(format!("0x{plug:08X}"));
            let mut variant = json!({"socket_index":socket.index,"choice_index":choice_index,
                "source_plug_hash":format!("0x{plug:08X}"),"name":choice.name,
                "description":choice.description,"replace_effects":true,
                "investment_stats":choice.lowered.stats,"additional_sandbox_perks":choice.lowered.stock,
                "sandbox_perks":choice.lowered.programs});
            if let Some(dependency) = choice
                .lowered
                .raw
                .iter()
                .find_map(|program| program.projectile_dependency())
            {
                variant["projectile"] = serde_json::to_value(dependency)?;
            }
            if let Some(icon) = &choice.icon {
                variant["icon"] =
                    json!({"source":"image","name":choice.name,"image":{"png_base64":icon}});
            }
            variants.push(variant);
            let mut report = choice.report.clone();
            report["socket"] = json!(socket.index);
            report["choice"] = json!(choice_index);
            report["reusable"] = json!(choice_index < reusable);
            report["template_plug"] = json!(format!("{plug:08X}"));
            translated.push(report);
        }
        if !plugs.is_empty() {
            let count = plugs.len();
            columns[socket.index] = json!({"choices":plugs,"socket_type":socket.socket_type});
            // Choices past the reusable ones exist only in the roll pool. A socket rolls as many
            // of them as the column had nodes it offered, one step per node as Rise of Iron did.
            if reusable < count {
                columns[socket.index]["reusable_choices"] = json!(reusable);
                columns[socket.index]["randomized_selection_program"] =
                    json!([{"opcode":11,"operand":reusable}]);
            }
        }
    }
    // Talent failures stay in the choice report. A weapon with no translated talents
    // still keeps its appearance and base behavior, with unused donor perk lanes disabled.
    let mut legacy = false;
    if signature.is_none()
        && host == Host::Weapon
        && let Some(socket) = &intrinsic
        && let Some(&plug) = socket.plugs.first()
    {
        columns[socket.index] =
            json!({"choices":[format!("0x{plug:08X}")],"socket_type":socket.socket_type});
        variants.push(json!({"socket_index":socket.index,"choice_index":0,
            "source_plug_hash":format!("0x{plug:08X}"),"name":LEGACY_FRAME,
            "description":LEGACY_FRAME_DESCRIPTION,"replace_effects":true,
            "investment_stats":[],"additional_sandbox_perks":[],"sandbox_perks":[],
            "icon":{"source":"texture","tag":LEGACY_FRAME_ICON}}));
        legacy = true;
    }
    let intrinsic_socket = intrinsic
        .as_ref()
        .filter(|_| signature.is_some() || legacy)
        .map(|socket| socket.index);
    let intrinsic = if signature.is_some() {
        "signature"
    } else if legacy {
        "legacy_frame"
    } else {
        "none"
    };
    let adaptation = if host == Host::Armor {
        "Supported source armor talents become private equipped socket choices. Source activation, removal, rearm and explicit player or triggering-weapon targets are retained through the bounded translators. Supported charge and script-parameter grants use private properties restricted to matching ability banks. Every action and grant must translate for a choice to be admitted. Donor perk columns and direct perks are cleared. Native base stats remain the baseline. Source stat tiers, progression and unqualified effects remain unsupported. Ornaments keep their separate appearance sockets."
    } else {
        "Source talent columns become private socket choices, with additional perk sockets when needed. A column offers the first supported step of each of its first four nodes, in row order, and every supported step joins a private randomized plug set the socket rolls from, since Rise of Iron rolled one step of each node. Offered talents are available immediately, with plain stat adjustments as equipped stat contributions. A fixed damage talent sets the weapon's damage type, and no socket offers an element. One that also carries other effects keeps them in its own column, without an element change that lasts while the weapon is equipped. Alternative damage steps would be damage mods, so the weapon keeps the first source step's damage type, and the base's damage mod socket is disabled unless a talent column needs it. A weapon whose talents set no element is Kinetic. On an exotic, a fixed perk named after the item, else a fixed first-column perk, else the grid's structural trait takes the intrinsic socket. Supported attached status companions remain part of their talent. Source progression and unsupported talents are not migrated. Unsupported talents are reported individually and do not block weapon import."
    };
    // Authored columns make the build curate the sockets the base keeps, such as a year-two
    // armor base's random stat intrinsic, so each needs a default.
    let mut columns = json!(columns);
    let mut native_defaults = Vec::new();
    crate::tiger::shadowkeep::fill_native_defaults(&native, &mut columns, &mut native_defaults)?;
    Ok((
        columns,
        json!(variants),
        json!({"translated":translated,"unsupported":unsupported,"merged":merged,"damage_type":damage,
        "damage_origin":damage_origin,
        "behaviors":if host == Host::Weapon {
            property_behaviors(&choices.iter().flat_map(|c| c.lowered.raw.iter().cloned()).collect::<Vec<_>>())
        } else {
            Vec::new()
        },
        "native_defaults":native_defaults,
        "status":if translated.is_empty() { "unsupported" } else if unsupported.is_empty() { "translated" } else { "partial" },
        "intrinsic":intrinsic,
        "intrinsic_socket":intrinsic_socket,
        "trait_column":trait_at,
        "empty_source_grid":nodes.is_empty(),
        "finished_perk_templates":{"plug":template_hash,"indices":templates},
        "socket_layout_lender":armor_lender.as_ref().map(|lender| &lender["weapon"]),
        "host":if host == Host::Armor { "armor" } else { "weapon" },
        "adaptation":adaptation,
        "gameplay_verified":false}),
        damage,
    ))
}
