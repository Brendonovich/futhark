use std::sync::Arc;

use futhark_graph::{
    ChannelCx, ChannelOp, Channels, Clock, CookError, Engine, GraphError, Network, OpId, OpSpec, Params, Registry,
    Value,
};

#[derive(Params)]
struct ValueParams {
    value: f64,
}

/// One sample holding `value`.
#[derive(Default)]
struct Constant;

impl ChannelOp for Constant {
    type Params = ValueParams;
    const SPEC: OpSpec = OpSpec {
        name: "channel/constant",
        label: "Constant",
        doc: "",
        inputs: 0..=0,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &ChannelCx<'_>, params: &ValueParams) -> Result<Channels, CookError> {
        let mut out = Channels::new(cx.time.rate, 0);
        out.push("v", vec![params.value as f32]);
        Ok(out)
    }
}

/// One sample per frame of the slice, holding the frame number.
#[derive(Default)]
struct Frames;

impl ChannelOp for Frames {
    type Params = ();
    const SPEC: OpSpec = OpSpec {
        name: "channel/frames",
        label: "Frames",
        doc: "",
        inputs: 0..=0,
        time_dependent: true,
    };

    fn cook(&mut self, cx: &ChannelCx<'_>, _: &()) -> Result<Channels, CookError> {
        let mut out = Channels::new(cx.time.rate, cx.time.slice_start);
        out.push("frame", cx.time.slice().map(|f| f as f32).collect());
        Ok(out)
    }
}

/// Sums the first sample of every input. Fails when `value` is negative.
#[derive(Default)]
struct Sum;

impl ChannelOp for Sum {
    type Params = ValueParams;
    const SPEC: OpSpec = OpSpec {
        name: "channel/sum",
        label: "Sum",
        doc: "",
        inputs: 1..=4,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &ChannelCx<'_>, params: &ValueParams) -> Result<Channels, CookError> {
        if params.value < 0.0 {
            return Err(CookError::new("negative"));
        }
        let total = cx
            .inputs()
            .iter()
            .map(|input| input.channels()[0].sample(0))
            .sum::<f32>();
        let mut out = Channels::new(cx.time.rate, 0);
        out.push("sum", vec![total + params.value as f32]);
        Ok(out)
    }
}

fn engine() -> Engine {
    let mut registry = Registry::new();
    registry.add_channel::<Constant>();
    registry.add_channel::<Frames>();
    registry.add_channel::<Sum>();
    Engine::new(Network::new(Arc::new(registry)), Clock::default())
}

fn add(engine: &mut Engine, name: &str, ty: &str) -> OpId {
    let network = engine.network_mut();
    network.add(network.root(), name, ty).unwrap()
}

fn first(engine: &mut Engine, id: OpId) -> f32 {
    engine.pull(id).unwrap().channels().unwrap().channels()[0].sample(0)
}

#[test]
fn cooks_only_when_pulled_and_only_once() {
    let mut e = engine();
    let a = add(&mut e, "a", "channel/constant");
    let sum = add(&mut e, "sum", "channel/sum");
    e.network_mut().connect(a, sum, 0).unwrap();
    assert_eq!((e.cooks(a), e.cooks(sum)), (0, 0));

    assert_eq!(first(&mut e, sum), 0.0);
    e.advance();
    first(&mut e, sum);
    assert_eq!((e.cooks(a), e.cooks(sum)), (1, 1));
}

#[test]
fn an_edit_recooks_only_what_depends_on_it() {
    let mut e = engine();
    let a = add(&mut e, "a", "channel/constant");
    let b = add(&mut e, "b", "channel/constant");
    let sum = add(&mut e, "sum", "channel/sum");
    e.network_mut().connect(a, sum, 0).unwrap();
    e.network_mut().connect(b, sum, 1).unwrap();
    first(&mut e, sum);

    e.network_mut().set_param(a, "value", Value::Float(2.0)).unwrap();
    assert_eq!(first(&mut e, sum), 2.0);
    assert_eq!((e.cooks(a), e.cooks(b), e.cooks(sum)), (2, 1, 2));

    // Setting the same value again changes nothing.
    e.network_mut().set_param(a, "value", Value::Float(2.0)).unwrap();
    first(&mut e, sum);
    assert_eq!((e.cooks(a), e.cooks(sum)), (2, 2));
}

#[test]
fn diamonds_cook_each_operator_once() {
    let mut e = engine();
    let a = add(&mut e, "a", "channel/frames");
    let left = add(&mut e, "left", "channel/sum");
    let right = add(&mut e, "right", "channel/sum");
    let sum = add(&mut e, "sum", "channel/sum");
    let net = e.network_mut();
    net.connect(a, left, 0).unwrap();
    net.connect(a, right, 0).unwrap();
    net.connect(left, sum, 0).unwrap();
    net.connect(right, sum, 1).unwrap();

    for _ in 0..3 {
        e.advance();
        first(&mut e, sum);
    }
    assert_eq!([a, left, right, sum].map(|id| e.cooks(id)), [3, 3, 3, 3]);
}

#[test]
fn time_dependent_operators_cover_skipped_frames() {
    let mut e = engine();
    let frames = add(&mut e, "frames", "channel/frames");
    let samples = |e: &mut Engine| {
        e.pull(frames).unwrap().channels().unwrap().channels()[0]
            .samples
            .clone()
    };

    assert_eq!(samples(&mut e), [0.0]);
    e.advance();
    assert_eq!(samples(&mut e), [1.0]);
    // Not pulled for three frames: the next cook covers all of them.
    e.set_frame(4);
    assert_eq!(samples(&mut e), [2.0, 3.0, 4.0]);
    // Catching up is capped at 200 ms (12 frames at 60 fps).
    e.set_frame(100);
    assert_eq!(samples(&mut e).len(), 12);
    // Jumping back starts a fresh slice.
    e.set_frame(10);
    assert_eq!(samples(&mut e), [10.0]);
}

#[test]
fn errors_propagate_and_recover() {
    let mut e = engine();
    let a = add(&mut e, "a", "channel/sum");
    let b = add(&mut e, "b", "channel/sum");
    let c = add(&mut e, "c", "channel/constant");
    e.network_mut().connect(a, b, 0).unwrap();

    assert_eq!(e.pull(a).unwrap_err().0, "needs at least 1 input");
    assert_eq!(e.pull(b).unwrap_err().0, "input a has an error");

    e.network_mut().connect(c, a, 0).unwrap();
    e.network_mut().set_param(a, "value", Value::Float(-1.0)).unwrap();
    assert_eq!(e.pull(a).unwrap_err().0, "negative");

    e.network_mut().set_param(a, "value", Value::Float(1.0)).unwrap();
    assert_eq!(first(&mut e, b), 1.0);
    assert!(e.error(a).is_none());
}

#[test]
fn removing_an_operator_disconnects_it() {
    let mut e = engine();
    let a = add(&mut e, "a", "channel/constant");
    let sum = add(&mut e, "sum", "channel/sum");
    e.network_mut().connect(a, sum, 0).unwrap();
    first(&mut e, sum);

    e.network_mut().remove(a).unwrap();
    assert!(e.network().node(sum).inputs().is_empty());
    assert_eq!(e.pull(sum).unwrap_err().0, "needs at least 1 input");
}

#[test]
fn wiring_rules() {
    let mut e = engine();
    let a = add(&mut e, "a", "channel/constant");
    let b = add(&mut e, "b", "channel/sum");
    let c = add(&mut e, "c", "channel/sum");
    let fx = add(&mut e, "fx", "component/base");
    let net = e.network_mut();
    let inner = net.add(fx, "inner", "channel/sum").unwrap();

    net.connect(b, c, 0).unwrap();
    assert!(matches!(net.connect(c, b, 0), Err(GraphError::Cycle { .. })));
    assert!(matches!(net.connect(b, b, 1), Err(GraphError::Cycle { .. })));
    assert!(matches!(net.connect(a, inner, 0), Err(GraphError::NotSiblings { .. })));
    assert!(matches!(net.connect(a, fx, 0), Err(GraphError::FamilyMismatch { .. })));
    assert!(matches!(net.connect(a, b, 4), Err(GraphError::TooManyInputs { .. })));
    assert!(matches!(
        net.add(net.root(), "a", "channel/sum"),
        Err(GraphError::DuplicateName(_))
    ));
    assert!(matches!(
        net.add(net.root(), "1a", "channel/sum"),
        Err(GraphError::InvalidName(_))
    ));
    assert!(matches!(
        net.add(a, "x", "channel/sum"),
        Err(GraphError::NotAComponent(_))
    ));
}

#[test]
fn paths() {
    let mut e = engine();
    let fx = add(&mut e, "fx", "component/base");
    let net = e.network_mut();
    let inner = net.add(fx, "inner", "channel/constant").unwrap();
    assert_eq!(net.path(net.root()), "/");
    assert_eq!(net.path(inner), "/fx/inner");
    assert_eq!(net.find("/fx/inner"), Some(inner));
    assert_eq!(net.find("/"), Some(net.root()));
    assert_eq!(net.find("/fx/missing"), None);

    net.rename(fx, "effects").unwrap();
    assert_eq!(net.find("/effects/inner"), Some(inner));
}
