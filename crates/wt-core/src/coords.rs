use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    model::{name_short, Geometry, PortMap, Target},
    session,
    settings::PortSettings,
    CoreError, ExitClass,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Coordinates {
    pub slot: u32,
    pub geometry: Geometry,
    pub ports: PortMap,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChooseResult {
    pub coordinates: Coordinates,
    pub notices: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TombstoneCoordinates {
    pub target: Target,
    pub coordinates: Coordinates,
}

#[derive(Clone, Debug)]
pub struct ChooseInput<'a> {
    pub target: &'a Target,
    pub allocated_ranges: &'a [(u32, u32)],
    pub allocated_slots: &'a BTreeSet<u32>,
    pub squatted: &'a BTreeSet<u32>,
    pub settings: PortSettings,
    pub tombstone: Option<&'a TombstoneCoordinates>,
    pub ports: PortMap,
    pub taken_name_shorts: &'a BTreeSet<String>,
    pub taken_sessions: &'a BTreeSet<String>,
}

pub fn choose(input: ChooseInput<'_>) -> Result<ChooseResult, CoreError> {
    if let Some(tombstone) = input.tombstone {
        if &tombstone.target == input.target {
            return Ok(ChooseResult {
                coordinates: tombstone.coordinates.clone(),
                notices: Vec::new(),
            });
        }
    }

    let max_slots = input.settings.max_slots()?;
    if input.ports.len() > usize::from(input.settings.stride) {
        return Err(CoreError::new(
            ExitClass::State,
            "CONFIG_INVALID",
            "declared ports exceed the allocated stride",
            "declare fewer ports or increase ports.stride",
        ));
    }
    // Both identities are functions of the address, so allocation only has to
    // establish that this address does not derive one another address already
    // holds; neither is recorded, because deriving them again always agrees.
    let short = name_short(input.target.label.as_str(), &input.target.name);
    let session_name = session::name(input.target.label.as_str(), &input.target.name);
    if input.taken_name_shorts.contains(&short) || input.taken_sessions.contains(&session_name) {
        return Err(CoreError::new(
            ExitClass::Conflict,
            "IDENTITY_COLLISION",
            "a derived coordinate identity collides with another address",
            "choose another tree name",
        ));
    }
    let mut notices = Vec::new();
    for slot in 0..max_slots {
        if input.allocated_slots.contains(&slot) || input.squatted.contains(&slot) {
            continue;
        }
        let geometry = input.settings.geometry(slot)?;
        let range = (
            u32::from(geometry.port_base),
            u32::from(geometry.port_base) + u32::from(geometry.stride) - 1,
        );
        if input
            .allocated_ranges
            .iter()
            .any(|used| range.0 <= used.1 && used.0 <= range.1)
        {
            if !notices.iter().any(|notice| notice == "GEOMETRY_CONFLICT") {
                notices.push("GEOMETRY_CONFLICT".to_owned());
            }
            continue;
        }
        return Ok(ChooseResult {
            coordinates: Coordinates {
                slot,
                geometry,
                ports: input.ports,
            },
            notices,
        });
    }
    Err(CoreError::new(
        ExitClass::Conflict,
        "SLOTS_EXHAUSTED",
        "no non-overlapping unsquatted port slot is available",
        "remove unused trees or expand the port geometry",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::PortName, ports};
    use proptest::prelude::*;

    fn input<'a>(
        target: &'a Target,
        allocated_ranges: &'a [(u32, u32)],
        allocated_slots: &'a BTreeSet<u32>,
        tombstone: Option<&'a TombstoneCoordinates>,
        taken: &'a BTreeSet<String>,
    ) -> ChooseInput<'a> {
        ChooseInput {
            target,
            allocated_ranges,
            allocated_slots,
            squatted: allocated_slots,
            settings: PortSettings::default(),
            tombstone,
            ports: PortMap::new(),
            taken_name_shorts: taken,
            taken_sessions: taken,
        }
    }

    proptest! {
        #[test]
        fn geometry_validation_uses_bounded_u32_arithmetic(base in any::<u16>(), stride in any::<u8>()) {
            let settings = PortSettings { base, stride };
            let expected_valid = base >= 1024
                && stride > 0
                && (65_536_u32 - u32::from(base)) / u32::from(stride.max(1)) >= 1;
            prop_assert_eq!(settings.max_slots().is_ok(), expected_valid);
            if expected_valid {
                prop_assert_eq!(settings.geometry(u32::MAX).unwrap_err().code.0, "SLOTS_EXHAUSTED");
            }
        }

        #[test]
        fn valid_geometry_slots_stay_inside_the_port_space(base in 1024u16..=65534, stride in 1u8..=255) {
            let settings = PortSettings { base, stride };
            if let Ok(max_slots) = settings.max_slots() {
                let last = settings.geometry(max_slots - 1).unwrap();
                prop_assert!(u32::from(last.port_base) + u32::from(last.stride) <= 65_536);
                prop_assert_eq!(settings.geometry(max_slots).unwrap_err().code.0, "SLOTS_EXHAUSTED");
            }
        }

        #[test]
        fn chosen_ranges_do_not_overlap(base in 1024u16..60000, stride in 1u8..64, occupied in prop::collection::btree_set(0u32..20, 0..10)) {
            let settings = PortSettings { base, stride };
            prop_assume!(settings.max_slots().is_ok());
            let ranges: Vec<_> = occupied.iter().filter_map(|slot| settings.geometry(*slot).ok()).map(|geometry| {
                (u32::from(geometry.port_base), u32::from(geometry.port_base) + u32::from(geometry.stride) - 1)
            }).collect();
            let target = Target::parse("repo/work").unwrap();
            let result = choose(ChooseInput {
                target: &target,
                allocated_ranges: &ranges,
                allocated_slots: &occupied,
                squatted: &BTreeSet::new(),
                settings,
                tombstone: None,
                ports: PortMap::new(),
                taken_name_shorts: &BTreeSet::new(),
                taken_sessions: &BTreeSet::new(),
            });
            if let Ok(chosen) = result {
                let chosen = chosen.coordinates;
                let range = (u32::from(chosen.geometry.port_base), u32::from(chosen.geometry.port_base) + u32::from(chosen.geometry.stride) - 1);
                prop_assert!(ranges.iter().all(|used| range.1 < used.0 || used.1 < range.0));
            }
        }
    }

    #[test]
    fn tombstone_inherits_the_complete_coordinate_record() {
        let target = Target::parse("repo/work").unwrap();
        let ports = ports::append(&PortMap::new(), &[PortName::new("http").unwrap()], 16)
            .unwrap()
            .ports;
        let coordinates = Coordinates {
            slot: 3,
            geometry: PortSettings::default().geometry(3).unwrap(),
            ports,
        };
        let tombstone = TombstoneCoordinates {
            target: target.clone(),
            coordinates: coordinates.clone(),
        };
        assert_eq!(
            choose(input(
                &target,
                &[(20_048, 20_063)],
                &BTreeSet::from([3]),
                Some(&tombstone),
                &BTreeSet::new(),
            ))
            .unwrap()
            .coordinates,
            coordinates
        );
    }

    #[test]
    fn smallest_free_slot_and_identity_collisions_are_reported() {
        let target = Target::parse("repo/work").unwrap();
        let occupied = BTreeSet::from([0, 2]);
        let chosen = choose(input(
            &target,
            &[(20_000, 20_015), (20_032, 20_047)],
            &occupied,
            None,
            &BTreeSet::new(),
        ))
        .unwrap();
        assert_eq!(chosen.coordinates.slot, 1);
        assert!(chosen.notices.is_empty());

        let conflicted = choose(input(
            &target,
            &[(20_000, 20_031)],
            &BTreeSet::from([0]),
            None,
            &BTreeSet::new(),
        ))
        .unwrap();
        assert_eq!(conflicted.coordinates.slot, 2);
        assert_eq!(conflicted.notices, ["GEOMETRY_CONFLICT"]);

        let collision = choose(input(
            &target,
            &[],
            &BTreeSet::new(),
            None,
            &BTreeSet::from([name_short("repo", "work")]),
        ))
        .unwrap_err();
        assert_eq!(collision.code.0, "IDENTITY_COLLISION");
    }

    #[test]
    fn persisted_and_squatted_ranges_are_both_avoided() {
        let target = Target::parse("repo/work").unwrap();
        let allocated = BTreeSet::from([0]);
        let squatted = BTreeSet::from([1]);
        let chosen = choose(ChooseInput {
            target: &target,
            allocated_ranges: &[(20_000, 20_015)],
            allocated_slots: &allocated,
            squatted: &squatted,
            settings: PortSettings::default(),
            tombstone: None,
            ports: PortMap::new(),
            taken_name_shorts: &BTreeSet::new(),
            taken_sessions: &BTreeSet::new(),
        })
        .unwrap();
        assert_eq!(chosen.coordinates.slot, 2);

        let exhausted = choose(ChooseInput {
            target: &target,
            allocated_ranges: &[(65_520, 65_535)],
            allocated_slots: &BTreeSet::from([0]),
            squatted: &BTreeSet::new(),
            settings: PortSettings {
                base: 65_520,
                stride: 16,
            },
            tombstone: None,
            ports: PortMap::new(),
            taken_name_shorts: &BTreeSet::new(),
            taken_sessions: &BTreeSet::new(),
        })
        .unwrap_err();
        assert_eq!(exhausted.code.0, "SLOTS_EXHAUSTED");
    }

    #[test]
    fn slot_independent_refusals_win_even_when_all_slots_are_taken() {
        let target = Target::parse("repo/work").unwrap();
        let settings = PortSettings {
            base: 65_520,
            stride: 16,
        };
        let full_ports: PortMap = (0..17)
            .map(|index| (PortName::new(format!("p{index}")).unwrap(), index))
            .collect();
        let error = choose(ChooseInput {
            target: &target,
            allocated_ranges: &[],
            allocated_slots: &BTreeSet::from([0]),
            squatted: &BTreeSet::new(),
            settings,
            tombstone: None,
            ports: full_ports,
            taken_name_shorts: &BTreeSet::new(),
            taken_sessions: &BTreeSet::new(),
        })
        .unwrap_err();
        assert_eq!(error.code.0, "CONFIG_INVALID");

        let error = choose(ChooseInput {
            target: &target,
            allocated_ranges: &[],
            allocated_slots: &BTreeSet::from([0]),
            squatted: &BTreeSet::new(),
            settings,
            tombstone: None,
            ports: PortMap::new(),
            taken_name_shorts: &BTreeSet::from([name_short("repo", "work")]),
            taken_sessions: &BTreeSet::new(),
        })
        .unwrap_err();
        assert_eq!(error.code.0, "IDENTITY_COLLISION");
    }

    #[test]
    fn non_matching_tombstone_does_not_supply_coordinates() {
        let target = Target::parse("repo/work").unwrap();
        let other = Target::parse("repo/other").unwrap();
        let tombstone = TombstoneCoordinates {
            target: other,
            coordinates: Coordinates {
                slot: 7,
                geometry: PortSettings::default().geometry(7).unwrap(),
                ports: PortMap::new(),
            },
        };
        let chosen = choose(input(
            &target,
            &[],
            &BTreeSet::new(),
            Some(&tombstone),
            &BTreeSet::new(),
        ))
        .unwrap();
        assert_eq!(chosen.coordinates.slot, 0);
    }
}
