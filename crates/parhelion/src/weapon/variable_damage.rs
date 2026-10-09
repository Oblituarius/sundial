//! Reload-hold element switching: The Fundamentals grafted onto another body.
//!
//! Proven in game on 2026-09-16. A SUROS Regime body with Hard Light as its appearance donor and
//! Hard Light's Fundamentals plug in its trait socket stepped Void, Arc, Solar under a held Reload
//! exactly like stock Hard Light: the plug's three action-state records flipped and the HUD pip
//! followed. Nothing else was needed, not Hard Light's item hash, runtime entity, pattern
//! selector, base perks or its declaration-only intrinsic perk 479. The hold follows the gear-art
//! row the presentation donor supplies, and the element change is the plug's three
//! predicate-gated Set Host Mode effects.
use super::*;

/// Hard Light, the weapon whose record drives the reload hold.
#[cfg(test)]
pub(crate) const HARD_LIGHT_ITEM_HASH: u32 = 0xF5DE_4480;
/// Hard Light's Fundamentals plug: stock rows 462 (Arc), 463 (Solar) and 464 (Void).
pub(crate) const FUNDAMENTALS_PLUG_HASH: u32 = 0x9C33_04DA;
/// The native socket type of weapon trait sockets, the lane the plug is pinned into.
pub(crate) const TRAIT_SOCKET_TYPE: u16 = 92;
/// The elements in the order the hold steps through them: selector 0, 1, 2, then 3.
pub(crate) const SELECTOR_ORDER: [ModernDamageType; 4] = [
    ModernDamageType::Void,
    ModernDamageType::Arc,
    ModernDamageType::Solar,
    ModernDamageType::Kinetic,
];

/// The stock finished-perk row on the plug that sets one element.
#[must_use]
pub(crate) const fn element_perk_index(element: ModernDamageType) -> Option<u16> {
    match element {
        ModernDamageType::Void => Some(464),
        ModernDamageType::Arc => Some(462),
        ModernDamageType::Solar => Some(463),
        ModernDamageType::Kinetic => None,
    }
}

#[must_use]
pub(crate) const fn element_label(element: ModernDamageType) -> &'static str {
    match element {
        ModernDamageType::Kinetic => "Kinetic",
        ModernDamageType::Arc => "Arc",
        ModernDamageType::Solar => "Solar",
        ModernDamageType::Void => "Void",
    }
}

/// The element the weapon rests on: the first chosen element in selector order.
#[must_use]
pub(crate) fn resting_element(elements: &[ModernDamageType]) -> Option<ModernDamageType> {
    SELECTOR_ORDER
        .into_iter()
        .find(|element| elements.contains(element))
}

/// The chosen elements in selector order, without repeats.
#[must_use]
pub(crate) fn ordered_elements(elements: &[ModernDamageType]) -> Vec<ModernDamageType> {
    SELECTOR_ORDER
        .into_iter()
        .filter(|element| elements.contains(element))
        .collect()
}

pub(crate) fn validate_elements(elements: &[ModernDamageType]) -> AuthoringResult<()> {
    let unique = ordered_elements(elements);
    if unique.len() != elements.len() {
        return Err(invalid("Variable damage lists an element twice."));
    }
    if unique.len() < 2 {
        return Err(invalid(
            "Variable damage needs at least two elements. One element is a fixed damage type.",
        ));
    }
    Ok(())
}

pub(super) fn validate_spec(spec: &WeaponCloneSpec) -> AuthoringResult<()> {
    let Some(variable) = &spec.overrides.variable_damage else {
        return Ok(());
    };
    validate_elements(&variable.elements)?;
    if let Some(resting) = spec.overrides.modern_damage_type
        && !variable.elements.contains(&resting)
    {
        return Err(invalid(format!(
            "Variable damage rests on {}, which is not one of its elements.",
            element_label(resting)
        )));
    }
    Ok(())
}

/// The expanded spec when the weapon switches elements, or `None` when nothing changes.
pub(super) fn expand_spec(
    spec: &WeaponCloneSpec,
    definition: &[u8],
) -> AuthoringResult<Option<WeaponCloneSpec>> {
    if spec.overrides.variable_damage.is_none() {
        return Ok(None);
    }
    let mut expanded = spec.clone();
    expanded.overrides = expand_overrides(&spec.overrides, &weapon_socket_types(definition)?)?;
    Ok(Some(expanded))
}

/// Builds the sandbox perk override for one element. Kinetic uses a custom program;
/// other elements use their stock finished-perk rows.
fn element_sandbox_perk(element: ModernDamageType) -> WeaponSandboxPerkRuntimeOverride {
    use sundial::package_authoring::sandbox_perk::program::{Action, Program, Trigger, NativeNode};
    match element_perk_index(element) {
        Some(index) => WeaponSandboxPerkRuntimeOverride {
            program: None,
            projectiles: Vec::new(),
            source_perk_index: index,
            activation: None,
            runtime_values: Vec::new(),
            action_float_values: Vec::new(),
        },
        None => {
            // Kinetic: use Borealis Arc (index 461) as the structural base — distinct from Hard
            // Light's Arc (462), Solar (463) and Void (464) so all four indices are unique when
            // placed in one variant. Override the program to Set Damage Type 0 (Kinetic) with the
            // same reload-hold trigger condition (18) the stock Fundamentals rows use.
            let reload_trigger = NativeNode::condition(18).expect("Reload trigger condition 18 missing");
            WeaponSandboxPerkRuntimeOverride {
                program: Some(Program {
                    name: "Set Damage Type: Kinetic".to_owned(),
                    trigger: Trigger::Native,
                    duration_ms: 0,
                    cooldown_ms: 0,
                    chance_permyriad: 0,
                    actions: vec![Action::set_damage_type(0)],
                    removal_key: None,
                    native_trigger: Some(reload_trigger),
                    native_removal: None,
                    native: None,
                    auxiliary: Vec::new(),
                    policy: None,
                    alternative_triggers: Vec::new(),
                    alternative_removals: Vec::new(),
                    native_rearm: None,
                    alternative_rearms: Vec::new(),
                    additional_groups: Vec::new(),
                }),
                projectiles: Vec::new(),
                source_perk_index: 461,
                activation: None,
                runtime_values: Vec::new(),
                action_float_values: Vec::new(),
            }
        }
    }
}

/// Pins The Fundamentals into the first trait socket and, for a subset of elements, adds a
/// private variant of the plug carrying only the chosen stock rows.
pub(crate) fn expand_overrides(
    overrides: &WeaponCloneOverrides,
    socket_types: &[u16],
) -> AuthoringResult<WeaponCloneOverrides> {
    let Some(variable) = &overrides.variable_damage else {
        return Ok(overrides.clone());
    };
    validate_elements(&variable.elements)?;
    // An author can turn one of the donor's columns into a trait socket, or append one, and can
    // put The Fundamentals there themselves. Read the lanes the way the socket list draws them
    // and use the socket they chose, so the build does not lead a second lane with the same perk.
    let lanes = crate::weapon_behavior::effective_socket_types(
        &crate::weapon_behavior::authored_socket_roles(overrides),
        socket_types,
    );
    let trait_lanes = || {
        lanes
            .iter()
            .enumerate()
            .filter(|(_, socket_type)| **socket_type == TRAIT_SOCKET_TYPE)
            .map(|(lane, _)| lane)
    };
    let placed = crate::weapon_behavior::authored_socket_choices(overrides);
    let lane = trait_lanes()
        .find(|lane| {
            placed
                .get(*lane)
                .is_some_and(|choices| choices.contains(&FUNDAMENTALS_PLUG_HASH))
        })
        .or_else(|| trait_lanes().next())
        .ok_or_else(|| {
            invalid("Variable damage needs a trait socket, and this weapon has none.")
        })?;
    let socket_index = u16::try_from(lane)
        .map_err(|_| invalid("Variable damage trait socket index does not fit 16 bits"))?;
    let mut expanded = overrides.clone();
    // The reload hold is carried by a behavior record in the weapon family's shared content
    // owner, not by the carrier's gear art. Request it by family so the compiler picks Hard
    // Light's record for rifles and Borealis's for snipers.
    // Encode the element count in the behavior ID so patches() can set the state-machine
    // cycling count to (element_count - 2). "element-switch" without a suffix defaults to 3
    // (Arc/Solar/Void); "element-switch:N" carries an explicit count for N > 3.
    let element_switch_id = format!(
        "{}:{}",
        crate::weapon_behavior::ELEMENT_SWITCH,
        variable.elements.len()
    );
    if !expanded
        .additional_behaviors
        .iter()
        .any(|id| crate::weapon_behavior::is_element_switch_id(id))
    {
        expanded.additional_behaviors.push(element_switch_id);
    }
    if expanded.socket_columns.is_empty() {
        expanded.socket_columns = vec![None; socket_types.len()];
    }
    if expanded.socket_columns.len() < socket_types.len() {
        return Err(invalid(format!(
            "The recipe lists {} socket columns but the base weapon has {} sockets.",
            expanded.socket_columns.len(),
            socket_types.len()
        )));
    }
    // The Fundamentals leads the lane rather than emptying it, so an author's own choices for
    // that socket stay behind it and the element still steps from the first plug.
    let column = expanded.socket_columns[lane].get_or_insert_with(|| WeaponSocketColumnOverride {
        choices: Vec::new(),
        socket_type: None,
        choice_weight_bits: Vec::new(),
        choice_conditions: Vec::new(),
        reusable_plug_set_index: None,
        randomized_plug_set_index: None,
        randomized_selection_program: Vec::new(),
    });
    column
        .choices
        .retain(|choice| *choice != FUNDAMENTALS_PLUG_HASH);
    column.choices.insert(0, FUNDAMENTALS_PLUG_HASH);
    // If the recipe already provides a custom variant at this socket's choice 0, honour it and
    // skip auto-generation. This is the intended path for a hand-authored 4-state perk (like one
    // using a full native graph for each element) that replaces the auto-generated variant.
    let user_has_variant = expanded
        .socket_plug_variants
        .iter()
        .any(|variant| variant.socket_index == socket_index && variant.choice_index == 0);
    let elements = ordered_elements(&variable.elements);
    // Stock plug has Arc/Solar/Void only. Need a private variant whenever Kinetic is included
    // or the recipe uses a subset of the three stock elements. All chosen elements — including
    // Kinetic's custom-program perk — go into one variant at choice_index 0, replacing the
    // stock plug's effects. A second variant at a different choice would require a second plug
    // hash in the socket column, colliding with any author-chosen choices already there.
    let has_kinetic = elements.iter().any(|e| *e == ModernDamageType::Kinetic);
    let stock_count = elements
        .iter()
        .filter(|e| element_perk_index(**e).is_some())
        .count();
    let needs_variant = !user_has_variant && (has_kinetic || stock_count < 3);
    if needs_variant {
        let labels = elements
            .iter()
            .map(|element| element_label(*element))
            .collect::<Vec<_>>();
        expanded
            .socket_plug_variants
            .push(WeaponSocketPlugVariantOverride {
                replace_effects: true,
                investment_stats: Vec::new(),
                socket_index,
                choice_index: 0,
                source_plug_hash: FUNDAMENTALS_PLUG_HASH,
                name: Some(format!("The Fundamentals ({})", labels.join(" / "))),
                icon: None,
                classification_donor_hash: None,
                description: Some(format!(
                    "Hold Reload to step the weapon's damage type through {}.",
                    labels.join(", ")
                )),
                additional_sandbox_perks: Vec::new(),
                sandbox_perks: elements
                    .iter()
                    .map(|element| element_sandbox_perk(*element))
                    .collect(),
            });
    }
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOCKET_TYPES: [u16; 8] = [176, 65, 177, TRAIT_SOCKET_TYPE, 183, u16::MAX, 454, 448];

    fn overrides(elements: &[ModernDamageType]) -> WeaponCloneOverrides {
        WeaponCloneOverrides {
            variable_damage: Some(WeaponVariableDamage {
                elements: elements.to_vec(),
            }),
            ..WeaponCloneOverrides::default()
        }
    }

    fn column(choice: u32) -> Option<WeaponSocketColumnOverride> {
        Some(WeaponSocketColumnOverride {
            choices: vec![choice],
            socket_type: None,
            choice_weight_bits: Vec::new(),
            choice_conditions: Vec::new(),
            reusable_plug_set_index: None,
            randomized_plug_set_index: None,
            randomized_selection_program: Vec::new(),
        })
    }

    #[test]
    fn every_element_pins_the_stock_plug_without_a_variant() {
        let expanded = expand_overrides(&overrides(&SELECTOR_ORDER), &SOCKET_TYPES).unwrap();
        assert_eq!(expanded.socket_columns.len(), SOCKET_TYPES.len());
        assert_eq!(expanded.socket_columns[3], column(FUNDAMENTALS_PLUG_HASH));
        assert!(
            expanded
                .socket_columns
                .iter()
                .enumerate()
                .all(|(lane, column)| lane == 3 || column.is_none())
        );
        // All 4 elements: one variant at choice 0 containing all perks (Kinetic via custom program)
        assert_eq!(expanded.socket_plug_variants.len(), 1);
        let variant = &expanded.socket_plug_variants[0];
        assert_eq!((variant.socket_index, variant.choice_index), (3, 0));
        assert_eq!(variant.source_plug_hash, FUNDAMENTALS_PLUG_HASH);
        // Void=464, Arc=462, Solar=463, Kinetic=461 — all four must be distinct or
        // set_weapon_base_sandbox_perks rejects the plug with a duplicate-row error.
        assert_eq!(
            variant.sandbox_perks.iter().map(|p| p.source_perk_index).collect::<Vec<_>>(),
            [464, 462, 463, 461],
        );
        let kinetic = variant.sandbox_perks.iter().find(|p| p.source_perk_index == 461).unwrap();
        assert!(kinetic.program.is_some());
    }

    #[test]
    fn a_subset_adds_a_private_variant_in_selector_order() {
        let expanded = expand_overrides(
            &overrides(&[ModernDamageType::Solar, ModernDamageType::Void]),
            &SOCKET_TYPES,
        )
        .unwrap();
        let [variant] = expanded.socket_plug_variants.as_slice() else {
            panic!("one variant");
        };
        assert!(variant.replace_effects);
        assert_eq!((variant.socket_index, variant.choice_index), (3, 0));
        assert_eq!(variant.source_plug_hash, FUNDAMENTALS_PLUG_HASH);
        assert_eq!(
            variant.name.as_deref(),
            Some("The Fundamentals (Void / Solar)")
        );
        assert_eq!(
            variant
                .sandbox_perks
                .iter()
                .map(|perk| perk.source_perk_index)
                .collect::<Vec<_>>(),
            [464, 463]
        );
        assert!(
            variant
                .sandbox_perks
                .iter()
                .all(|perk| perk.program.is_none())
        );
    }

    #[test]
    fn element_count_is_encoded_in_the_behavior_id() {
        // The behavior ID carries the element count so patches() can set the state-machine
        // cycling count to (element_count - 2). 3 elements → "element-switch:3" (count=1),
        // 4 elements → "element-switch:4" (count=2, enabling state 3 for Kinetic).
        let expanded_3 = expand_overrides(
            &overrides(&[ModernDamageType::Void, ModernDamageType::Arc, ModernDamageType::Solar]),
            &SOCKET_TYPES,
        )
        .unwrap();
        assert!(expanded_3.additional_behaviors.iter().any(|id| id == "element-switch:3"));

        let expanded_4 = expand_overrides(&overrides(&SELECTOR_ORDER), &SOCKET_TYPES).unwrap();
        assert!(expanded_4.additional_behaviors.iter().any(|id| id == "element-switch:4"));
    }

    #[test]
    fn existing_columns_keep_their_other_lanes() {
        let mut source = overrides(&SELECTOR_ORDER);
        source.socket_columns = vec![None; SOCKET_TYPES.len()];
        source.socket_columns[1] = column(0x1234_5678);
        let expanded = expand_overrides(&source, &SOCKET_TYPES).unwrap();
        assert_eq!(expanded.socket_columns[1], source.socket_columns[1]);
        assert_eq!(expanded.socket_columns[3], column(FUNDAMENTALS_PLUG_HASH));
    }

    #[test]
    fn a_chosen_trait_lane_keeps_its_choices_behind_the_fundamentals() {
        let mut chosen = overrides(&SELECTOR_ORDER);
        chosen.socket_columns = vec![None; SOCKET_TYPES.len()];
        chosen.socket_columns[3] = column(0x1234_5678);
        let expanded = expand_overrides(&chosen, &SOCKET_TYPES).unwrap();
        assert_eq!(
            expanded.socket_columns[3]
                .as_ref()
                .map(|column| column.choices.as_slice()),
            Some([FUNDAMENTALS_PLUG_HASH, 0x1234_5678].as_slice())
        );
    }

    #[test]
    fn a_user_variant_at_choice_0_suppresses_auto_generation() {
        // A recipe that provides its own variant at the trait socket's choice 0 is accepted:
        // variable_damage honours it instead of generating its own, so the user can supply a
        // hand-authored 4-state perk (e.g. with full native graphs for each element).
        let mut occupied = overrides(&SELECTOR_ORDER);
        occupied
            .socket_plug_variants
            .push(WeaponSocketPlugVariantOverride {
                replace_effects: true,
                investment_stats: Vec::new(),
                socket_index: 3,
                choice_index: 0,
                source_plug_hash: FUNDAMENTALS_PLUG_HASH,
                name: None,
                icon: None,
                classification_donor_hash: None,
                description: None,
                additional_sandbox_perks: Vec::new(),
                sandbox_perks: Vec::new(),
            });
        let expanded = expand_overrides(&occupied, &SOCKET_TYPES).unwrap();
        // The user's variant is the only one — no auto-generated variant was added.
        assert_eq!(expanded.socket_plug_variants.len(), 1);
        assert_eq!(expanded.socket_plug_variants[0].source_plug_hash, FUNDAMENTALS_PLUG_HASH);

        // A weapon with no trait socket is still an error.
        assert!(expand_overrides(&overrides(&SELECTOR_ORDER), &[176, 65, 177]).is_err());

        // A column list too short for the socket types is still an error.
        let mut narrow = overrides(&SELECTOR_ORDER);
        narrow.socket_columns = vec![None; 2];
        assert!(expand_overrides(&narrow, &SOCKET_TYPES).is_err());
    }

    #[test]
    fn element_sets_are_validated() {
        assert!(validate_elements(&SELECTOR_ORDER).is_ok());
        assert!(validate_elements(&[ModernDamageType::Arc, ModernDamageType::Void]).is_ok());
        assert!(validate_elements(&[ModernDamageType::Kinetic, ModernDamageType::Arc]).is_ok());
        assert!(validate_elements(&[ModernDamageType::Kinetic, ModernDamageType::Solar, ModernDamageType::Void]).is_ok());
        assert!(validate_elements(&[ModernDamageType::Arc]).is_err());
        assert!(validate_elements(&[ModernDamageType::Arc, ModernDamageType::Arc]).is_err());
        assert_eq!(
            resting_element(&[ModernDamageType::Solar, ModernDamageType::Arc]),
            Some(ModernDamageType::Arc)
        );
        assert_eq!(
            resting_element(&[ModernDamageType::Kinetic, ModernDamageType::Arc]),
            Some(ModernDamageType::Arc)
        );
        assert_eq!(
            resting_element(&[ModernDamageType::Kinetic, ModernDamageType::Void]),
            Some(ModernDamageType::Void)
        );
        assert_eq!(
            resting_element(&[ModernDamageType::Kinetic]),
            Some(ModernDamageType::Kinetic)
        );
        assert_eq!(resting_element(&[]), None);
    }
}
