//! Each tree owns a block of `stride` ports starting at
//! `base + slot * stride`. Named ports take offsets within the block in the
//! order they are first declared.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, TcpListener};

use crate::error::{Error, Result};
use crate::settings::PortGeometry;
use crate::state::State;

pub fn port(geometry: PortGeometry, slot: u32, offset: u16) -> u16 {
    (u32::from(geometry.base) + slot * u32::from(geometry.stride) + u32::from(offset)) as u16
}

/// The lowest slot no tree holds and whose ports nothing on the machine is
/// listening on.
pub fn allocate_slot(state: &State, geometry: PortGeometry) -> Result<u32> {
    let slots = (65_536 - u32::from(geometry.base)) / u32::from(geometry.stride);
    (0..slots)
        .filter(|slot| !state.trees.iter().any(|tree| tree.slot == *slot))
        .find(|slot| block_is_free(geometry, *slot))
        .ok_or_else(|| {
            Error::Conflict("no free block of ports is left; remove some trees".to_owned())
        })
}

fn block_is_free(geometry: PortGeometry, slot: u32) -> bool {
    (0..geometry.stride).all(|offset| {
        let port = port(geometry, slot, offset);
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
            && TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
    })
}

/// Gives every name in `wanted` an offset, keeping existing ones. Returns
/// whether anything was added.
pub fn assign(
    offsets: &mut BTreeMap<String, u16>,
    wanted: &[String],
    geometry: PortGeometry,
) -> Result<bool> {
    let mut changed = false;
    for name in wanted {
        if offsets.contains_key(name) {
            continue;
        }
        let next = offsets.values().max().map_or(0, |max| max + 1);
        if next >= geometry.stride {
            return Err(Error::Config(format!(
                "port `{name}` doesn't fit: a tree has {} ports (ports.stride in ~/.wt/config.toml)",
                geometry.stride
            )));
        }
        offsets.insert(name.clone(), next);
        changed = true;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_append_only() {
        let geometry = PortGeometry {
            base: 20000,
            stride: 3,
        };
        let mut offsets = BTreeMap::new();
        assign(&mut offsets, &["b".into(), "a".into()], geometry).unwrap();
        assert_eq!(offsets["b"], 0);
        assert_eq!(offsets["a"], 1);
        // Dropping `b` from the config and adding `c` never reuses b's port.
        assert!(assign(&mut offsets, &["a".into(), "c".into()], geometry).unwrap());
        assert_eq!(offsets["c"], 2);
        assert!(assign(&mut offsets, &["d".into()], geometry).is_err());
    }

    #[test]
    fn port_numbers() {
        let geometry = PortGeometry {
            base: 20000,
            stride: 16,
        };
        assert_eq!(port(geometry, 2, 3), 20035);
    }
}
