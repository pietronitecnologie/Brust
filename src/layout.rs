//! Tile geometry calculation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Layout {
    /// A single pane.
    #[serde(rename = "1")]
    Single,
    /// Two panes side by side.
    #[serde(rename = "2h")]
    SplitH,
    /// Two panes stacked.
    #[serde(rename = "2v")]
    SplitV,
    /// Three columns.
    #[serde(rename = "3c")]
    Cols3,
    /// One large on the left, two stacked on the right.
    #[serde(rename = "3l")]
    MainLeft,
    /// One large on top, two side by side below.
    #[serde(rename = "3t")]
    MainTop,
    /// 2x2 grid.
    #[serde(rename = "4")]
    Grid,
}

pub const ALL: [Layout; 7] = [
    Layout::Single,
    Layout::SplitH,
    Layout::SplitV,
    Layout::Cols3,
    Layout::MainLeft,
    Layout::MainTop,
    Layout::Grid,
];

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct R {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl R {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        R { x, y, w: w.max(0.0), h: h.max(0.0) }
    }
}

/// Draggable splitter. The new value is `(pos - min) / (max - min)`,
/// where `pos` is the pointer coordinate along `axis`.
#[derive(Debug, Clone, Serialize)]
pub struct Splitter {
    pub index: usize,
    pub axis: char,
    pub rect: R,
    pub min: f64,
    pub max: f64,
}

const MIN_RATIO: f64 = 0.08;

impl Layout {
    pub fn id(self) -> &'static str {
        match self {
            Layout::Single => "1",
            Layout::SplitH => "2h",
            Layout::SplitV => "2v",
            Layout::Cols3 => "3c",
            Layout::MainLeft => "3l",
            Layout::MainTop => "3t",
            Layout::Grid => "4",
        }
    }

    pub fn from_id(s: &str) -> Option<Layout> {
        ALL.into_iter().find(|l| l.id() == s)
    }

    pub fn pane_count(self) -> usize {
        match self {
            Layout::Single => 1,
            Layout::SplitH | Layout::SplitV => 2,
            Layout::Cols3 | Layout::MainLeft | Layout::MainTop => 3,
            Layout::Grid => 4,
        }
    }

    pub fn default_ratios(self) -> Vec<f64> {
        match self {
            Layout::Single => vec![],
            Layout::SplitH | Layout::SplitV => vec![0.5],
            Layout::Cols3 => vec![1.0 / 3.0, 2.0 / 3.0],
            Layout::MainLeft | Layout::MainTop => vec![0.6, 0.5],
            Layout::Grid => vec![0.5, 0.5],
        }
    }

    /// Normalizes a ratios vector (length and bounds).
    pub fn sanitize(self, ratios: &[f64]) -> Vec<f64> {
        let def = self.default_ratios();
        let mut v: Vec<f64> = def
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let r = ratios.get(i).copied().filter(|r| r.is_finite()).unwrap_or(*d);
                r.clamp(MIN_RATIO, 1.0 - MIN_RATIO)
            })
            .collect();
        if self == Layout::Cols3 {
            v[0] = v[0].min(1.0 - 2.0 * MIN_RATIO);
            v[1] = v[1].max(v[0] + MIN_RATIO);
        }
        v
    }

    /// Computes the pane and splitter rectangles within the given area.
    pub fn compute(self, a: R, ratios: &[f64], gap: f64) -> (Vec<R>, Vec<Splitter>) {
        let r = self.sanitize(ratios);
        let g = gap / 2.0;
        let vsplit = |area: R, ratio: f64, index: usize| {
            let x = area.x + area.w * ratio;
            (
                R::new(area.x, area.y, x - g - area.x, area.h),
                R::new(x + g, area.y, area.x + area.w - x - g, area.h),
                Splitter { index, axis: 'x', rect: R::new(x - g, area.y, gap, area.h), min: area.x, max: area.x + area.w },
            )
        };
        let hsplit = |area: R, ratio: f64, index: usize| {
            let y = area.y + area.h * ratio;
            (
                R::new(area.x, area.y, area.w, y - g - area.y),
                R::new(area.x, y + g, area.w, area.y + area.h - y - g),
                Splitter { index, axis: 'y', rect: R::new(area.x, y - g, area.w, gap), min: area.y, max: area.y + area.h },
            )
        };
        match self {
            Layout::Single => (vec![a], vec![]),
            Layout::SplitH => {
                let (l, rr, s) = vsplit(a, r[0], 0);
                (vec![l, rr], vec![s])
            }
            Layout::SplitV => {
                let (t, b, s) = hsplit(a, r[0], 0);
                (vec![t, b], vec![s])
            }
            Layout::Cols3 => {
                let x1 = a.x + a.w * r[0];
                let x2 = a.x + a.w * r[1];
                let p = vec![
                    R::new(a.x, a.y, x1 - g - a.x, a.h),
                    R::new(x1 + g, a.y, x2 - x1 - gap, a.h),
                    R::new(x2 + g, a.y, a.x + a.w - x2 - g, a.h),
                ];
                let s = vec![
                    Splitter { index: 0, axis: 'x', rect: R::new(x1 - g, a.y, gap, a.h), min: a.x, max: a.x + a.w },
                    Splitter { index: 1, axis: 'x', rect: R::new(x2 - g, a.y, gap, a.h), min: a.x, max: a.x + a.w },
                ];
                (p, s)
            }
            Layout::MainLeft => {
                let (l, right, s0) = vsplit(a, r[0], 0);
                let (t, b, s1) = hsplit(right, r[1], 1);
                (vec![l, t, b], vec![s0, s1])
            }
            Layout::MainTop => {
                let (top, bottom, s0) = hsplit(a, r[0], 0);
                let (l, rr, s1) = vsplit(bottom, r[1], 1);
                (vec![top, l, rr], vec![s0, s1])
            }
            Layout::Grid => {
                let x = a.x + a.w * r[0];
                let y = a.y + a.h * r[1];
                let lw = x - g - a.x;
                let rw = a.x + a.w - x - g;
                let th = y - g - a.y;
                let bh = a.y + a.h - y - g;
                let p = vec![
                    R::new(a.x, a.y, lw, th),
                    R::new(x + g, a.y, rw, th),
                    R::new(a.x, y + g, lw, bh),
                    R::new(x + g, y + g, rw, bh),
                ];
                let s = vec![
                    Splitter { index: 0, axis: 'x', rect: R::new(x - g, a.y, gap, a.h), min: a.x, max: a.x + a.w },
                    Splitter { index: 1, axis: 'y', rect: R::new(a.x, y - g, a.w, gap), min: a.y, max: a.y + a.h },
                ];
                (p, s)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_area() {
        let (p, s) = Layout::Grid.compute(R::new(0.0, 0.0, 100.0, 100.0), &[0.5, 0.5], 6.0);
        assert_eq!(p.len(), 4);
        assert_eq!(s.len(), 2);
        assert_eq!(p[0], R::new(0.0, 0.0, 47.0, 47.0));
        assert_eq!(p[3], R::new(53.0, 53.0, 47.0, 47.0));
    }

    #[test]
    fn sanitize_cols3() {
        let r = Layout::Cols3.sanitize(&[0.7, 0.2]);
        assert!(r[1] > r[0]);
        assert_eq!(Layout::SplitH.sanitize(&[]), vec![0.5]);
        assert_eq!(Layout::SplitH.sanitize(&[2.0]), vec![1.0 - MIN_RATIO]);
    }

    #[test]
    fn ids_roundtrip() {
        for l in ALL {
            assert_eq!(Layout::from_id(l.id()), Some(l));
            assert_eq!(l.compute(R::new(0.0, 0.0, 10.0, 10.0), &[], 1.0).0.len(), l.pane_count());
        }
    }
}
