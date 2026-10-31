/// The data of the Channel family: named channels of samples that share a sample rate and a start.
#[derive(Clone, Debug, PartialEq)]
pub struct Channels {
    /// Samples per second.
    pub rate: f64,
    /// Index of the first sample, at `rate`. Time-sliced operators start at the first frame of their slice.
    pub start: i64,
    channels: Vec<Channel>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub samples: Vec<f32>,
}

impl Channel {
    /// The sample at `index`, holding the last sample past the end. An empty channel reads as 0.
    pub fn sample(&self, index: usize) -> f32 {
        self.samples.get(index).or(self.samples.last()).copied().unwrap_or(0.0)
    }
}

impl Channels {
    pub fn new(rate: f64, start: i64) -> Self {
        Channels {
            rate,
            start,
            channels: Vec::new(),
        }
    }

    /// Adds a channel. Every channel has the same number of samples.
    pub fn push(&mut self, name: impl Into<String>, samples: Vec<f32>) {
        if let Some(first) = self.channels.first() {
            assert_eq!(
                first.samples.len(),
                samples.len(),
                "every channel has the same number of samples"
            );
        }
        self.channels.push(Channel {
            name: name.into(),
            samples,
        });
    }

    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    pub fn get(&self, name: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.name == name)
    }

    /// Samples per channel.
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, |c| c.samples.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
