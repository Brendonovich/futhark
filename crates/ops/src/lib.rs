//! The built-in operators.

pub mod channel;

use futhark_graph::Registry;

/// A registry with every built-in operator type.
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry.add_channel::<channel::Constant>();
    registry.add_channel::<channel::Lfo>();
    registry.add_channel::<channel::Math>();
    registry.add_channel::<channel::Noise>();
    registry
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use futhark_graph::{ParamKind, valid_name};

    #[test]
    fn schemas_are_valid() {
        for op in super::registry().iter() {
            let (family, name) = op.name.split_once('/').expect("type names are family/name");
            assert_eq!(family, op.family.to_string().to_lowercase(), "{}", op.name);
            assert!(valid_name(name), "{}", op.name);

            let mut seen = HashSet::new();
            for spec in &op.params {
                let at = format!("{}.{}", op.name, spec.name);
                assert!(seen.insert(spec.name), "{at} is declared twice");
                // `input` lines hold wires in project files.
                assert!(
                    valid_name(spec.name) && spec.name != "input",
                    "{at} is not a usable name"
                );
                assert_eq!(
                    spec.coerce(spec.default.clone()).as_ref(),
                    Ok(&spec.default),
                    "{at}'s default"
                );
                if let ParamKind::Menu(items) = spec.kind {
                    for item in items {
                        assert!(valid_name(item.name), "{at} has menu item {:?}", item.name);
                    }
                }
            }
        }
    }
}
