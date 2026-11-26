//! Small animation helpers: eased tweens like the CSS `transition: … 1s
//! cubic-bezier(.2,.8,.2,1)`, and a pulse. Windows only redraw at 60 fps while one runs.

use std::time::{Duration, Instant};

/// CSS cubic-bezier(x1, y1, x2, y2) evaluated at time fraction t.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    // Solve x(s) = t by bisection (robust and plenty fast for a handful of tweens).
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if bez(x1, x2, mid) < t { lo = mid } else { hi = mid }
    }
    bez(y1, y2, (lo + hi) / 2.0)
}

pub fn ease(t: f32) -> f32 {
    cubic_bezier(0.2, 0.8, 0.2, 1.0, t)
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur: Duration,
    set: bool,
}

impl Tween {
    pub fn new(dur_ms: u64) -> Tween {
        Tween { from: 0.0, to: 0.0, start: Instant::now(), dur: Duration::from_millis(dur_ms), set: false }
    }

    /// Animate towards `v`. The first value is taken as-is unless `animate_first`.
    pub fn set(&mut self, v: f32, animate_first: bool) {
        if !self.set {
            self.set = true;
            self.from = if animate_first { 0.0 } else { v };
            self.to = v;
            self.start = Instant::now();
            return;
        }
        if (v - self.to).abs() < 1e-4 {
            return;
        }
        self.from = self.value();
        self.to = v;
        self.start = Instant::now();
    }

    /// Restart from `from` (e.g. the card's count-up when it opens).
    pub fn replay_from(&mut self, from: f32) {
        self.from = from;
        self.start = Instant::now();
    }

    pub fn value(&self) -> f32 {
        let t = self.start.elapsed().as_secs_f32() / self.dur.as_secs_f32().max(1e-3);
        self.from + (self.to - self.from) * ease(t)
    }

    pub fn running(&self) -> bool {
        self.set && self.start.elapsed() < self.dur && (self.to - self.from).abs() > 1e-4
    }
}

/// CSS `@keyframes pulse { 0%,100% {opacity 1} 50% {opacity .55} }` over 1.6 s, ease-in-out.
pub fn pulse(since: Instant) -> f32 {
    let p = (since.elapsed().as_secs_f32() % 1.6) / 1.6;
    let tri = if p < 0.5 { p * 2.0 } else { (1.0 - p) * 2.0 };
    let e = cubic_bezier(0.42, 0.0, 0.58, 1.0, tri);
    1.0 - 0.45 * e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints_and_shape() {
        assert!(ease(0.0).abs() < 1e-3);
        assert!((ease(1.0) - 1.0).abs() < 1e-3);
        assert!(ease(0.25) > 0.6, "fast start, slow end");
    }
}
