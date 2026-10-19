//! Shared by the views read from a plugin's controls (eq_curve, dynamics,
//! scope): a control read through the points it is known at.

/// `x` through the (x, y) `points`, by x: straight lines between them and
/// carried on past both ends (`x` itself with fewer than two)
pub fn interpolate(points: &[(f32, f32)], x: f32) -> f32 {
    if points.len() < 2 {
        return x;
    }
    let i = points
        .windows(2)
        .position(|w| x <= w[1].0)
        .unwrap_or(points.len() - 2);
    let ((x0, y0), (x1, y1)) = (points[i], points[i + 1]);
    if x1 == x0 {
        y0
    } else {
        y0 + (y1 - y0) * (x - x0) / (x1 - x0)
    }
}

/// Control values by symbol, for tests
#[cfg(test)]
pub(crate) fn values(pairs: &'static [(&'static str, f32)]) -> impl Fn(&str) -> Option<f32> {
    move |s| pairs.iter().find(|p| p.0 == s).map(|p| p.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_are_joined_and_carried_on() {
        let points = [(0.0, 0.5), (0.7, 0.707), (1.0, 1.0)];
        assert_eq!(interpolate(&points, 0.0), 0.5);
        assert!((interpolate(&points, 0.85) - 0.8535).abs() < 1e-3);
        assert!(interpolate(&points, 1.4) > 1.0);
        assert!(interpolate(&points, -0.7) < 0.5);
        assert_eq!(interpolate(&[], 3.0), 3.0);
    }
}
