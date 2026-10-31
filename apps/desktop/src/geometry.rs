//! The network editor's geometry: the camera, node boxes, connectors, wires and hit testing. All in world points
//! (the units of an operator's `position`) unless a name says screen.

/// A node's box: a 16:9 preview over a title bar. `position` is its top-left corner.
pub const NODE_W: f32 = 100.0;
pub const PREVIEW_H: f32 = 56.0;
pub const TITLE_H: f32 = 18.0;
pub const NODE_H: f32 = PREVIEW_H + TITLE_H;
/// A connector's diameter.
pub const CONNECTOR: f32 = 10.0;
/// How far from a connector's centre a dropped wire still lands on it.
pub const CONNECTOR_REACH: f32 = 12.0;
/// Below this zoom, nodes are too small for previews to be worth drawing or streaming.
pub const MIN_PREVIEW_ZOOM: f32 = 0.45;
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 4.0;

/// Where the world shows in the canvas: `screen = world * zoom + pan`, from the canvas's top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub pan: [f32; 2],
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            pan: [40.0, 40.0],
            zoom: 1.0,
        }
    }
}

impl Camera {
    pub fn to_screen(self, p: [f32; 2]) -> [f32; 2] {
        [p[0] * self.zoom + self.pan[0], p[1] * self.zoom + self.pan[1]]
    }

    pub fn to_world(self, q: [f32; 2]) -> [f32; 2] {
        [(q[0] - self.pan[0]) / self.zoom, (q[1] - self.pan[1]) / self.zoom]
    }

    pub fn panned(&self, by: [f32; 2]) -> Camera {
        Camera {
            pan: [self.pan[0] + by[0], self.pan[1] + by[1]],
            zoom: self.zoom,
        }
    }

    /// Zoomed by `factor` about screen point `q`, which keeps showing the same world point.
    pub fn zoomed_about(&self, q: [f32; 2], factor: f32) -> Camera {
        let p = self.to_world(q);
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        Camera {
            pan: [q[0] - p[0] * zoom, q[1] - p[1] * zoom],
            zoom,
        }
    }

    /// Shows `bounds` (x0, y0, x1, y1) centred in a viewport of `size`, at zoom 1 or smaller if it doesn't fit.
    pub fn framing(bounds: [f32; 4], size: [f32; 2]) -> Camera {
        const MARGIN: f32 = 60.0;
        let (w, h) = (bounds[2] - bounds[0], bounds[3] - bounds[1]);
        let fit = ((size[0] - 2.0 * MARGIN) / w.max(1.0)).min((size[1] - 2.0 * MARGIN) / h.max(1.0));
        let zoom = fit.clamp(MIN_ZOOM, 1.0);
        let centre = [(bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0];
        Camera {
            pan: [size[0] / 2.0 - centre[0] * zoom, size[1] / 2.0 - centre[1] * zoom],
            zoom,
        }
    }

    pub fn shows_previews(&self) -> bool {
        self.zoom >= MIN_PREVIEW_ZOOM
    }

    /// Whether any of world rect `r` (x0, y0, x1, y1) shows in a viewport of `size`.
    pub fn sees(&self, r: [f32; 4], size: [f32; 2]) -> bool {
        let a = self.to_screen([r[0], r[1]]);
        let b = self.to_screen([r[2], r[3]]);
        a[0] < size[0] && a[1] < size[1] && b[0] > 0.0 && b[1] > 0.0
    }

    /// A node's preview size in device pixels at this zoom, for a display of `scale` pixels per point. The width
    /// rounds up to a multiple of 32, so a pinch asks the server for new sizes now and then rather than every frame.
    pub fn preview_pixels(&self, scale: f32) -> [u32; 2] {
        const STEP: f32 = 32.0;
        let w = ((NODE_W * self.zoom * scale) / STEP).ceil().max(1.0) * STEP;
        [w as u32, (w * PREVIEW_H / NODE_W).round() as u32]
    }
}

/// A node's box (x0, y0, x1, y1).
pub fn node_rect(position: [f32; 2]) -> [f32; 4] {
    [position[0], position[1], position[0] + NODE_W, position[1] + NODE_H]
}

/// How many input connectors a node shows: every wired slot, plus one free slot while the type takes more, and at
/// least its minimum.
pub fn input_slots(wired: usize, min: usize, max: usize) -> usize {
    (wired + 1).max(min).min(max)
}

/// An input connector's centre, spread down the node's left edge.
pub fn input_point(position: [f32; 2], index: usize, slots: usize) -> [f32; 2] {
    let step = NODE_H / (slots as f32 + 1.0);
    [position[0], position[1] + step * (index as f32 + 1.0)]
}

/// The output connector's centre, on the right edge.
pub fn output_point(position: [f32; 2]) -> [f32; 2] {
    [position[0] + NODE_W, position[1] + NODE_H / 2.0]
}

/// A wire's cubic Bézier from an output to an input: it leaves rightward and arrives from the left.
pub fn wire(from: [f32; 2], to: [f32; 2]) -> [[f32; 2]; 4] {
    let reach = ((to[0] - from[0]).abs() * 0.5).max(40.0);
    [from, [from[0] + reach, from[1]], [to[0] - reach, to[1]], to]
}

/// The box (x0, y0, x1, y1) around every wire's control points, which contains the curves.
pub fn wires_bounds(wires: &[[[f32; 2]; 4]]) -> Option<[f32; 4]> {
    let mut points = wires.iter().flatten();
    let first = points.next()?;
    Some(points.fold([first[0], first[1], first[0], first[1]], |b, p| {
        [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]
    }))
}

/// The box (x0, y0, x1, y1) around nodes at `positions`.
pub fn nodes_bounds(positions: impl IntoIterator<Item = [f32; 2]>) -> Option<[f32; 4]> {
    positions
        .into_iter()
        .map(node_rect)
        .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

/// A node as hit testing sees it.
pub struct Target<'a> {
    pub position: [f32; 2],
    /// Input slots shown, and which are wired.
    pub inputs: &'a [bool],
}

/// Where a wire dropped at world point `p` connects: the nearest input connector within reach, otherwise the first
/// free input of the node under `p` (or its last, when all are wired). Returns the node's index and the input.
pub fn drop_target(p: [f32; 2], targets: &[Target<'_>]) -> Option<(usize, usize)> {
    let mut best: Option<(f32, usize, usize)> = None;
    for (i, t) in targets.iter().enumerate() {
        for slot in 0..t.inputs.len() {
            let c = input_point(t.position, slot, t.inputs.len());
            let d = ((c[0] - p[0]).powi(2) + (c[1] - p[1]).powi(2)).sqrt();
            if d <= CONNECTOR_REACH && best.is_none_or(|(bd, _, _)| d < bd) {
                best = Some((d, i, slot));
            }
        }
    }
    if let Some((_, i, slot)) = best {
        return Some((i, slot));
    }
    let (i, t) = targets
        .iter()
        .enumerate()
        .rev()
        .find(|(_, t)| contains(node_rect(t.position), p) && !t.inputs.is_empty())?;
    let slot = t.inputs.iter().position(|wired| !wired).unwrap_or(t.inputs.len() - 1);
    Some((i, slot))
}

/// How to show the picture at `rect` (x, y, w, h, in pixels) of an atlas of `atlas` pixels inside a `size` box,
/// keeping its shape and centring it: the picture's box within `size`, and where the whole atlas goes relative to
/// that box (both x, y, w, h in points). Clipping the atlas to the picture's box shows just the picture; the box
/// keeps half a pixel inside the picture's edges, where filtering would blend in its neighbours.
pub fn atlas_crop(rect: [u32; 4], atlas: [u32; 2], size: [f32; 2]) -> ([f32; 4], [f32; 4]) {
    let [x, y, w, h] = rect.map(|v| v as f32);
    let k = (size[0] / w.max(1.0)).min(size[1] / h.max(1.0));
    let (pw, ph) = (w * k, h * k);
    let inset = (k / 2.0).min(pw / 4.0).min(ph / 4.0);
    let (left, top) = ((size[0] - pw) / 2.0, (size[1] - ph) / 2.0);
    let picture = [left + inset, top + inset, pw - 2.0 * inset, ph - 2.0 * inset];
    let whole = [-x * k - inset, -y * k - inset, atlas[0] as f32 * k, atlas[1] as f32 * k];
    (picture, whole)
}

pub fn contains(r: [f32; 4], p: [f32; 2]) -> bool {
    p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn the_camera_maps_both_ways() {
        let cam = Camera {
            pan: [30.0, -12.0],
            zoom: 1.5,
        };
        let p = [42.0, 7.0];
        assert!(near(cam.to_world(cam.to_screen(p)), p));
        assert_eq!(cam.to_screen([0.0, 0.0]), [30.0, -12.0]);
        assert_eq!(cam.panned([5.0, 5.0]).pan, [35.0, -7.0]);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let cam = Camera::default();
        let q = [220.0, 130.0];
        let before = cam.to_world(q);
        let zoomed = cam.zoomed_about(q, 2.0);
        assert_eq!(zoomed.zoom, 2.0);
        assert!(near(zoomed.to_world(q), before));
        assert_eq!(cam.zoomed_about(q, 1000.0).zoom, MAX_ZOOM);
        assert_eq!(cam.zoomed_about(q, 0.0).zoom, MIN_ZOOM);
    }

    #[test]
    fn framing_centres_the_network() {
        let cam = Camera::framing([0.0, 0.0, 260.0, 234.0], [800.0, 600.0]);
        assert_eq!(cam.zoom, 1.0);
        assert!(near(cam.to_screen([130.0, 117.0]), [400.0, 300.0]));
        let wide = Camera::framing([0.0, 0.0, 4000.0, 100.0], [800.0, 600.0]);
        assert!(wide.zoom < 1.0);
        assert!(wide.sees([0.0, 0.0, 1.0, 1.0], [800.0, 600.0]));
        assert!(wide.sees([3999.0, 0.0, 4000.0, 1.0], [800.0, 600.0]));
    }

    #[test]
    fn culling_and_preview_sizes_follow_the_zoom() {
        let cam = Camera {
            pan: [0.0, 0.0],
            zoom: 1.0,
        };
        let size = [400.0, 300.0];
        assert!(cam.sees(node_rect([10.0, 10.0]), size));
        assert!(cam.sees(node_rect([-50.0, 10.0]), size), "partly showing");
        assert!(!cam.sees(node_rect([500.0, 10.0]), size));
        assert!(!cam.sees(node_rect([10.0, -200.0]), size));
        assert_eq!(cam.preview_pixels(2.0), [224, 125]);
        assert_eq!(
            cam.zoomed_about([0.0, 0.0], 1.05).preview_pixels(2.0),
            [224, 125],
            "small zooms ask for nothing new"
        );
        assert_eq!(cam.zoomed_about([0.0, 0.0], 0.1).preview_pixels(1.0), [32, 18]);
        let far = cam.zoomed_about([0.0, 0.0], 0.4);
        assert!(!far.shows_previews());
        assert!(cam.shows_previews());
    }

    #[test]
    fn connectors_sit_on_the_edges() {
        assert_eq!(input_slots(0, 0, 0), 0);
        assert_eq!(input_slots(0, 1, 1), 1);
        assert_eq!(input_slots(2, 0, usize::MAX), 3);
        assert_eq!(input_slots(2, 2, 2), 2);
        let one = input_point([0.0, 0.0], 0, 1);
        assert_eq!(one, [0.0, NODE_H / 2.0]);
        assert_eq!(output_point([10.0, 20.0]), [10.0 + NODE_W, 20.0 + NODE_H / 2.0]);
        let (a, b) = (input_point([0.0, 0.0], 0, 2), input_point([0.0, 0.0], 1, 2));
        assert!(a[1] < b[1] && a[1] > 0.0 && b[1] < NODE_H);
    }

    #[test]
    fn wires_leave_right_and_arrive_from_the_left() {
        let w = wire([100.0, 37.0], [300.0, 80.0]);
        assert_eq!(w[0], [100.0, 37.0]);
        assert_eq!(w[3], [300.0, 80.0]);
        assert!(w[1][0] > w[0][0] && w[1][1] == w[0][1]);
        assert!(w[2][0] < w[3][0] && w[2][1] == w[3][1]);
        // Backwards wires still bow out of each end.
        let back = wire([300.0, 0.0], [0.0, 0.0]);
        assert!(back[1][0] > 300.0 && back[2][0] < 0.0);
        assert_eq!(wires_bounds(&[w, back]), Some([-150.0, 0.0, 450.0, 80.0]));
        assert_eq!(wires_bounds(&[]), None);
    }

    #[test]
    fn atlas_crops_show_one_picture() {
        // A 200×100 picture at (400, 50) of a 2048×512 atlas, in a 100×56 box: half scale, letterboxed.
        let (picture, whole) = atlas_crop([400, 50, 200, 100], [2048, 512], [100.0, 56.0]);
        assert_eq!(picture, [0.25, 3.25, 99.5, 49.5]);
        assert_eq!(whole, [-200.25, -25.25, 1024.0, 256.0]);
        // The picture's first pixel lands on the box's edge: the atlas sits 200 points left of it, past the inset.
        assert_eq!(picture[0] + whole[0], -200.0);
        // A tall picture pillarboxes.
        let (picture, _) = atlas_crop([0, 0, 50, 100], [2048, 512], [100.0, 56.0]);
        let expected = [36.28, 0.28, 27.44, 55.44];
        assert!(
            picture.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-4),
            "{picture:?}"
        );
    }

    #[test]
    fn dropped_wires_find_their_input() {
        let targets = [
            Target {
                position: [0.0, 0.0],
                inputs: &[],
            },
            Target {
                position: [200.0, 0.0],
                inputs: &[true, false],
            },
        ];
        let second = input_point([200.0, 0.0], 1, 2);
        assert_eq!(drop_target([second[0] + 3.0, second[1] - 2.0], &targets), Some((1, 1)));
        let first = input_point([200.0, 0.0], 0, 2);
        assert_eq!(drop_target(first, &targets), Some((1, 0)));
        // On the body: the first free input.
        assert_eq!(drop_target([250.0, 10.0], &targets), Some((1, 1)));
        // A node without inputs, and empty space, take nothing.
        assert_eq!(drop_target([50.0, 30.0], &targets), None);
        assert_eq!(drop_target([150.0, 300.0], &targets), None);
        assert_eq!(
            nodes_bounds([[0.0, 0.0], [200.0, 10.0]]),
            Some([0.0, 0.0, 300.0, 10.0 + NODE_H])
        );
    }
}
