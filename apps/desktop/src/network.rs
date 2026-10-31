//! Reading a snapshot: operator paths, the operators of one component, and names for new ones.

use futhark_protocol::{Family, Op, OpType, Snapshot};

/// The component an operator lives in: `/fx` for `/fx/noise1`, `/` for `/lfo1`.
pub fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// The last part of a path: `noise1` for `/fx/noise1`.
pub fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A child's path: `/fx` and `noise1` make `/fx/noise1`.
pub fn join(component: &str, name: &str) -> String {
    if component == "/" {
        format!("/{name}")
    } else {
        format!("{component}/{name}")
    }
}

/// The components from the root down to `component`, as (label, path): `root`, then each part.
pub fn breadcrumb(component: &str) -> Vec<(String, String)> {
    let mut crumbs = vec![("root".to_string(), "/".to_string())];
    let mut path = String::new();
    for part in component.split('/').filter(|p| !p.is_empty()) {
        path.push('/');
        path.push_str(part);
        crumbs.push((part.to_string(), path.clone()));
    }
    crumbs
}

/// The operators directly inside `component`.
pub fn children<'a>(snapshot: &'a Snapshot, component: &'a str) -> impl Iterator<Item = &'a Op> + 'a {
    snapshot.ops.iter().filter(move |op| parent(&op.path) == component)
}

pub fn find<'a>(snapshot: &'a Snapshot, path: &str) -> Option<&'a Op> {
    snapshot.ops.iter().find(|op| op.path == path)
}

pub fn op_type<'a>(types: &'a [OpType], name: &str) -> Option<&'a OpType> {
    types.iter().find(|t| t.name == name)
}

/// A free name in `component` for a new operator of type `ty`: its last part and the lowest unused number, such as
/// `lfo2` for `channel/lfo`.
pub fn fresh_name(snapshot: &Snapshot, component: &str, ty: &str) -> String {
    let base: String = name(ty)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let taken: Vec<&str> = children(snapshot, component).map(|op| name(&op.path)).collect();
    (1..)
        .map(|n| format!("{base}{n}"))
        .find(|candidate| !taken.contains(&candidate.as_str()))
        .expect("some number is free")
}

pub const FAMILIES: [Family; 6] = [
    Family::Texture,
    Family::Channel,
    Family::Geometry,
    Family::Data,
    Family::Shader,
    Family::Component,
];

pub fn family_label(family: Family) -> &'static str {
    match family {
        Family::Texture => "Texture",
        Family::Channel => "Channel",
        Family::Geometry => "Geometry",
        Family::Data => "Data",
        Family::Shader => "Shader",
        Family::Component => "Component",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::snapshot;

    #[test]
    fn paths_split_and_join() {
        assert_eq!(parent("/lfo1"), "/");
        assert_eq!(parent("/fx/wobble"), "/fx");
        assert_eq!(name("/fx/wobble"), "wobble");
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/fx", "a"), "/fx/a");
        assert_eq!(
            breadcrumb("/fx/inner"),
            [("root", "/"), ("fx", "/fx"), ("inner", "/fx/inner")].map(|(a, b)| (a.to_string(), b.to_string()))
        );
    }

    #[test]
    fn children_stay_in_their_component() {
        let s = snapshot();
        let root: Vec<&str> = children(&s, "/").map(|op| op.path.as_str()).collect();
        assert_eq!(root, ["/fx", "/lfo1", "/mix", "/noise1", "/ramp1"]);
        let fx: Vec<&str> = children(&s, "/fx").map(|op| op.path.as_str()).collect();
        assert_eq!(fx, ["/fx/wobble"]);
    }

    #[test]
    fn new_names_are_free() {
        let s = snapshot();
        assert_eq!(fresh_name(&s, "/", "channel/lfo"), "lfo2");
        assert_eq!(fresh_name(&s, "/fx", "channel/lfo"), "lfo1");
        assert_eq!(fresh_name(&s, "/", "texture/file-in"), "file_in1");
    }
}
