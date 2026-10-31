//! A fixed network and type list (the `hello` example as the server shows it), for headless tests and scenes.

use futhark_protocol::{Family, MenuItem, Op, OpType, Param, ParamKind, ParamType, ProjectInfo, Snapshot, Value};

fn op(path: &str, ty: &str, family: Family, inputs: &[&str], position: [f32; 2]) -> Op {
    Op {
        path: path.into(),
        ty: ty.into(),
        family,
        inputs: inputs.iter().map(|i| Some(i.to_string())).collect(),
        params: Vec::new(),
        position,
        error: None,
    }
}

/// The `hello` example as the server shows it.
pub fn snapshot() -> Snapshot {
    let mut lfo = op("/lfo1", "channel/lfo", Family::Channel, &[], [0.0, 0.0]);
    lfo.params = vec![
        Param {
            name: "wave".into(),
            value: Value::String("sine".into()),
        },
        Param {
            name: "frequency".into(),
            value: Value::Float(0.5),
        },
        Param {
            name: "name".into(),
            value: Value::String("chan1".into()),
        },
    ];
    let mut noise = op("/noise1", "channel/noise", Family::Channel, &[], [0.0, 80.0]);
    noise.error = Some("seed out of range".into());
    Snapshot {
        revision: 3,
        project: ProjectInfo {
            path: "/tmp/hello".into(),
            name: "hello".into(),
            rate: 60.0,
        },
        ops: vec![
            op("/fx", "component/base", Family::Component, &[], [0.0, 160.0]),
            op("/fx/wobble", "channel/lfo", Family::Channel, &[], [0.0, 0.0]),
            lfo,
            op(
                "/mix",
                "channel/math",
                Family::Channel,
                &["lfo1", "noise1"],
                [160.0, 40.0],
            ),
            noise,
            op("/ramp1", "texture/ramp", Family::Texture, &[], [160.0, 160.0]),
        ],
    }
}

fn param(name: &str, kind: ParamKind, default: Value) -> ParamType {
    ParamType {
        name: name.into(),
        label: name.into(),
        doc: String::new(),
        kind,
        default,
        min: Some(0.0),
        max: None,
        soft_min: None,
        soft_max: Some(10.0),
    }
}

fn ty(name: &str, family: Family, inputs: (usize, usize), params: Vec<ParamType>) -> OpType {
    OpType {
        name: name.into(),
        label: name.rsplit('/').next().unwrap().into(),
        doc: String::new(),
        family,
        min_inputs: inputs.0,
        max_inputs: inputs.1,
        time_dependent: false,
        params,
    }
}

pub fn types() -> Vec<OpType> {
    vec![
        ty(
            "channel/lfo",
            Family::Channel,
            (0, 0),
            vec![
                param(
                    "wave",
                    ParamKind::Menu {
                        items: vec![
                            MenuItem {
                                name: "sine".into(),
                                label: "Sine".into(),
                            },
                            MenuItem {
                                name: "square".into(),
                                label: "Square".into(),
                            },
                        ],
                    },
                    Value::String("sine".into()),
                ),
                param("frequency", ParamKind::Float, Value::Float(1.0)),
                param("name", ParamKind::String, Value::String("chan1".into())),
            ],
        ),
        ty("channel/noise", Family::Channel, (0, 0), Vec::new()),
        ty(
            "channel/math",
            Family::Channel,
            (0, usize::MAX),
            vec![param("enabled", ParamKind::Toggle, Value::Bool(true))],
        ),
        ty("component/base", Family::Component, (0, 0), Vec::new()),
        ty("texture/ramp", Family::Texture, (0, 1), Vec::new()),
    ]
}
