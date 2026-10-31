//! Channel operators.

use std::f64::consts::TAU;

use futhark_graph::{ChannelCx, ChannelOp, Channels, CookError, Menu, OpSpec, Params};

#[derive(Params)]
pub struct ConstantParams {
    /// Name of the output channel.
    #[param(default = "chan1")]
    pub name: String,
    pub value: f64,
}

#[derive(Default)]
pub struct Constant;

impl ChannelOp for Constant {
    type Params = ConstantParams;
    const SPEC: OpSpec = OpSpec {
        name: "channel/constant",
        label: "Constant",
        doc: "One channel holding a single value.",
        inputs: 0..=0,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &ChannelCx<'_>, params: &ConstantParams) -> Result<Channels, CookError> {
        let mut out = Channels::new(cx.time.rate, 0);
        out.push(&params.name, vec![params.value as f32]);
        Ok(out)
    }
}

#[derive(Menu, Clone, Copy, Debug, PartialEq)]
pub enum Wave {
    Sine,
    Triangle,
    Square,
    /// Rises from 0 to 1, then drops back.
    Ramp,
}

impl Wave {
    /// The wave at `phase` (0 to 1), from -1 to 1 except for the ramp.
    fn at(self, phase: f64) -> f64 {
        match self {
            Wave::Sine => (phase * TAU).sin(),
            Wave::Triangle if phase < 0.25 => phase * 4.0,
            Wave::Triangle if phase < 0.75 => 2.0 - phase * 4.0,
            Wave::Triangle => phase * 4.0 - 4.0,
            Wave::Square if phase < 0.5 => 1.0,
            Wave::Square => -1.0,
            Wave::Ramp => phase,
        }
    }
}

#[derive(Params)]
pub struct LfoParams {
    pub wave: Wave,
    /// Cycles per second.
    #[param(default = 1.0, min = 0.0, soft_max = 10.0)]
    pub frequency: f64,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0)]
    pub amplitude: f64,
    pub offset: f64,
    /// Shifts the wave by a fraction of a cycle.
    #[param(soft_min = 0.0, soft_max = 1.0)]
    pub phase: f64,
    /// Name of the output channel.
    #[param(default = "chan1")]
    pub name: String,
}

#[derive(Default)]
pub struct Lfo;

impl ChannelOp for Lfo {
    type Params = LfoParams;
    const SPEC: OpSpec = OpSpec {
        name: "channel/lfo",
        label: "LFO",
        doc: "A repeating wave over time, one sample per frame.",
        inputs: 0..=0,
        time_dependent: true,
    };

    fn cook(&mut self, cx: &ChannelCx<'_>, p: &LfoParams) -> Result<Channels, CookError> {
        let samples = cx
            .time
            .slice()
            .map(|frame| {
                let phase = (cx.time.seconds_at(frame) * p.frequency + p.phase).rem_euclid(1.0);
                (p.wave.at(phase) * p.amplitude + p.offset) as f32
            })
            .collect();
        let mut out = Channels::new(cx.time.rate, cx.time.slice_start);
        out.push(&p.name, samples);
        Ok(out)
    }
}

#[derive(Params)]
pub struct NoiseParams {
    pub seed: i32,
    /// Seconds between noise features.
    #[param(default = 1.0, min = 0.001, soft_max = 10.0)]
    pub period: f64,
    /// Layers of finer noise added on top.
    #[param(default = 1, min = 1, max = 8)]
    pub octaves: i32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0)]
    pub amplitude: f64,
    pub offset: f64,
    /// Name of the output channel.
    #[param(default = "chan1")]
    pub name: String,
}

#[derive(Default)]
pub struct Noise;

impl ChannelOp for Noise {
    type Params = NoiseParams;
    const SPEC: OpSpec = OpSpec {
        name: "channel/noise",
        label: "Noise",
        doc: "Smooth random values over time, from -1 to 1 before amplitude and offset, one sample per frame.",
        inputs: 0..=0,
        time_dependent: true,
    };

    fn cook(&mut self, cx: &ChannelCx<'_>, p: &NoiseParams) -> Result<Channels, CookError> {
        let samples = cx
            .time
            .slice()
            .map(|frame| {
                let x = cx.time.seconds_at(frame) / p.period;
                (fractal_noise(p.seed as u32, x, p.octaves as u32) * p.amplitude + p.offset) as f32
            })
            .collect();
        let mut out = Channels::new(cx.time.rate, cx.time.slice_start);
        out.push(&p.name, samples);
        Ok(out)
    }
}

/// Value noise from -1 to 1, with `octaves` layers each twice as fine and half as strong.
fn fractal_noise(seed: u32, x: f64, octaves: u32) -> f64 {
    let (mut sum, mut weight, mut total, mut scale) = (0.0, 1.0, 0.0, 1.0);
    for octave in 0..octaves {
        sum += weight * value_noise(seed.wrapping_add(octave), x * scale);
        total += weight;
        weight *= 0.5;
        scale *= 2.0;
    }
    sum / total
}

fn value_noise(seed: u32, x: f64) -> f64 {
    let cell = x.floor();
    let t = x - cell;
    let smooth = t * t * (3.0 - 2.0 * t);
    let (a, b) = (lattice(seed, cell as i64), lattice(seed, cell as i64 + 1));
    a + (b - a) * smooth
}

/// A random value from -1 to 1 for each integer point (SplitMix64).
fn lattice(seed: u32, i: i64) -> f64 {
    let mut z = (i as u64) ^ (u64::from(seed) << 32);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

#[derive(Menu, Clone, Copy, Debug, PartialEq)]
pub enum Combine {
    Add,
    Subtract,
    Multiply,
    Maximum,
    Minimum,
}

impl Combine {
    fn apply(self, a: f32, b: f32) -> f32 {
        match self {
            Combine::Add => a + b,
            Combine::Subtract => a - b,
            Combine::Multiply => a * b,
            Combine::Maximum => a.max(b),
            Combine::Minimum => a.min(b),
        }
    }
}

#[derive(Params)]
pub struct MathParams {
    /// How inputs are combined, channel by channel in order.
    pub combine: Combine,
    /// Multiplies the result.
    #[param(default = 1.0, soft_min = -1.0, soft_max = 1.0)]
    pub gain: f64,
    /// Added after the gain.
    #[param(soft_min = -1.0, soft_max = 1.0)]
    pub offset: f64,
}

#[derive(Default)]
pub struct Math;

impl ChannelOp for Math {
    type Params = MathParams;
    const SPEC: OpSpec = OpSpec {
        name: "channel/math",
        label: "Math",
        doc: "Combines its inputs channel by channel, then applies a gain and offset. Channels are matched by \
              order and take the first input's names. A shorter channel holds its last sample.",
        inputs: 1..=16,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &ChannelCx<'_>, p: &MathParams) -> Result<Channels, CookError> {
        let (first, rest) = cx.inputs().split_first().expect("Math needs an input");
        let len = cx.inputs().iter().map(|input| input.len()).max().unwrap_or(0);
        let mut out = Channels::new(first.rate, first.start);
        for (index, channel) in first.channels().iter().enumerate() {
            let samples = (0..len)
                .map(|i| {
                    let value = rest
                        .iter()
                        .filter_map(|input| input.channels().get(index))
                        .fold(channel.sample(i), |acc, other| p.combine.apply(acc, other.sample(i)));
                    (f64::from(value) * p.gain + p.offset) as f32
                })
                .collect();
            out.push(channel.name.clone(), samples);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futhark_graph::{Clock, Engine, Network, OpId, Value};

    fn engine() -> Engine {
        Engine::new(Network::new(Arc::new(crate::registry())), Clock::default())
    }

    fn add(e: &mut Engine, name: &str, ty: &str, params: &[(&str, Value)]) -> OpId {
        let net = e.network_mut();
        let id = net.add(net.root(), name, ty).unwrap();
        for (param, value) in params {
            net.set_param(id, param, value.clone()).unwrap();
        }
        id
    }

    fn samples(e: &mut Engine, id: OpId) -> Vec<f32> {
        e.pull(id).unwrap().channels().unwrap().channels()[0].samples.clone()
    }

    #[test]
    fn lfo_waves() {
        let mut e = engine();
        // At 4 Hz and 60 fps, 15 frames make one cycle.
        let lfo = add(&mut e, "lfo", "channel/lfo", &[("frequency", Value::Float(4.0))]);
        let at = |e: &mut Engine, frame| {
            e.set_frame(frame);
            *samples(e, lfo).last().unwrap()
        };
        assert!(at(&mut e, 0).abs() < 1e-6);
        assert!((at(&mut e, 15) - 0.0).abs() < 1e-5);

        e.network_mut()
            .set_param(lfo, "wave", Value::Menu("square".into()))
            .unwrap();
        assert_eq!(at(&mut e, 3), 1.0);
        assert_eq!(at(&mut e, 9), -1.0);

        e.network_mut()
            .set_param(lfo, "wave", Value::Menu("ramp".into()))
            .unwrap();
        e.network_mut().set_param(lfo, "amplitude", Value::Float(2.0)).unwrap();
        e.network_mut().set_param(lfo, "offset", Value::Float(1.0)).unwrap();
        assert!((at(&mut e, 30) - 1.0).abs() < 1e-5);
        assert!((at(&mut e, 33) - 1.4).abs() < 1e-5);
    }

    #[test]
    fn noise_is_smooth_bounded_and_seeded() {
        let mut e = engine();
        let noise = add(&mut e, "noise", "channel/noise", &[("octaves", Value::Int(3))]);
        e.set_frame(11);
        e.pull(noise).unwrap();
        e.set_frame(400);
        let a = samples(&mut e, noise);
        assert_eq!(a.len(), 12);
        assert!(a.iter().all(|v| (-1.0..=1.0).contains(v)));
        assert!(a.windows(2).all(|w| (w[0] - w[1]).abs() < 0.2), "{a:?}");

        e.network_mut().set_param(noise, "seed", Value::Int(7)).unwrap();
        e.set_frame(400);
        assert_ne!(samples(&mut e, noise).last(), a.last());
    }

    #[test]
    fn math_combines_inputs() {
        let mut e = engine();
        let a = add(&mut e, "a", "channel/constant", &[("value", Value::Float(3.0))]);
        let b = add(&mut e, "b", "channel/constant", &[("value", Value::Float(4.0))]);
        let math = add(
            &mut e,
            "math",
            "channel/math",
            &[
                ("combine", Value::Menu("multiply".into())),
                ("gain", Value::Float(0.5)),
                ("offset", Value::Float(1.0)),
            ],
        );
        e.network_mut().connect(a, math, 0).unwrap();
        e.network_mut().connect(b, math, 1).unwrap();
        assert_eq!(samples(&mut e, math), [7.0]);
    }

    #[test]
    fn math_holds_shorter_inputs() {
        let mut e = engine();
        let lfo = add(&mut e, "lfo", "channel/lfo", &[("wave", Value::Menu("ramp".into()))]);
        let one = add(&mut e, "one", "channel/constant", &[("value", Value::Float(1.0))]);
        let math = add(&mut e, "math", "channel/math", &[]);
        e.network_mut().connect(lfo, math, 0).unwrap();
        e.network_mut().connect(one, math, 1).unwrap();
        e.pull(math).unwrap();
        e.set_frame(3);
        let out = samples(&mut e, math);
        let expected = [1.0 + 1.0 / 60.0, 1.0 + 2.0 / 60.0, 1.0 + 3.0 / 60.0];
        assert!(out.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-5), "{out:?}");
    }
}
