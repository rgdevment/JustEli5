//! How far behind the speech the live captions run, and how it changes.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LagSummary {
    pub samples: usize,
    pub median_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct LagModel {
    /// `(caption time, lag)` ordered by time.
    samples: Vec<(f64, f64)>,
    default_ms: f64,
    neighbours: usize,
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

impl LagModel {
    /// `samples` are `(when the caption appeared, caption time minus speech time)`.
    /// Samples far from the overall median are mismatches and are dropped.
    pub fn new(
        mut samples: Vec<(f64, f64)>,
        default_ms: f64,
        neighbours: usize,
        outlier_ms: f64,
    ) -> Self {
        samples.retain(|(t, lag)| t.is_finite() && lag.is_finite());
        if samples.len() >= 3 {
            let mut lags: Vec<f64> = samples.iter().map(|s| s.1).collect();
            let centre = median(&mut lags);
            samples.retain(|s| (s.1 - centre).abs() <= outlier_ms);
        } else {
            // Too few to tell a typical lag from a mismatch: only believe
            // samples close to what captions usually do.
            samples.retain(|s| (s.1 - default_ms).abs() <= outlier_ms);
        }
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self {
            samples,
            default_ms,
            neighbours: neighbours.max(1),
        }
    }

    /// The lag around time `t`: the median of the nearest samples.
    pub fn at(&self, t: f64) -> f64 {
        if self.samples.is_empty() {
            return self.default_ms;
        }
        let mut near: Vec<&(f64, f64)> = self.samples.iter().collect();
        near.sort_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs()));
        near.truncate(self.neighbours);
        median(&mut near.iter().map(|s| s.1).collect::<Vec<_>>())
    }

    pub fn summary(&self) -> LagSummary {
        let mut lags: Vec<f64> = self.samples.iter().map(|s| s.1).collect();
        if lags.is_empty() {
            return LagSummary {
                samples: 0,
                median_ms: None,
                min_ms: None,
                max_ms: None,
            };
        }
        let min = lags.iter().copied().fold(f64::INFINITY, f64::min);
        let max = lags.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        LagSummary {
            samples: lags.len(),
            median_ms: Some(median(&mut lags)),
            min_ms: Some(min),
            max_ms: Some(max),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_samples_means_the_default() {
        let model = LagModel::new(vec![], 1200.0, 5, 1500.0);
        assert_eq!(model.at(10_000.0), 1200.0);
        assert_eq!(model.summary().samples, 0);
    }

    #[test]
    fn mismatches_are_dropped_and_drift_is_followed() {
        // Lag grows from 800 ms to 2600 ms over 100 s; two wild mismatches.
        let mut samples: Vec<(f64, f64)> = (0..=10)
            .map(|i| (i as f64 * 10_000.0, 800.0 + i as f64 * 180.0))
            .collect();
        samples.push((15_000.0, 7_000.0));
        samples.push((55_000.0, -4_000.0));
        let model = LagModel::new(samples, 1200.0, 3, 1500.0);
        assert_eq!(model.summary().samples, 11);
        assert!((model.at(0.0) - 890.0).abs() < 100.0, "{}", model.at(0.0));
        assert!(
            (model.at(100_000.0) - 2510.0).abs() < 100.0,
            "{}",
            model.at(100_000.0)
        );
        assert!(model.at(50_000.0) > 1600.0 && model.at(50_000.0) < 1900.0);
    }

    #[test]
    fn with_two_samples_only_plausible_ones_count() {
        let model = LagModel::new(
            vec![(1_000.0, 1_300.0), (9_000.0, 5_900.0)],
            1200.0,
            5,
            1500.0,
        );
        assert_eq!(model.summary().samples, 1);
        assert_eq!(model.at(0.0), 1300.0);
        let none = LagModel::new(vec![(1.0, 6_000.0)], 1200.0, 5, 1500.0);
        assert_eq!(none.at(0.0), 1200.0);
    }

    #[test]
    fn non_finite_samples_are_ignored() {
        let model = LagModel::new(
            vec![(f64::NAN, 1.0), (1.0, f64::INFINITY), (2.0, 900.0)],
            1200.0,
            3,
            1500.0,
        );
        assert_eq!(model.at(0.0), 900.0);
    }
}
