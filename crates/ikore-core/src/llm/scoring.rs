use serde::{Deserialize, Serialize};

pub const HEALTH_WEIGHT: f64 = 0.30;
pub const LATENCY_WEIGHT: f64 = 0.20;
pub const RELIABILITY_WEIGHT: f64 = 0.20;
pub const COST_WEIGHT: f64 = 0.15;
pub const TASK_FIT_WEIGHT: f64 = 0.10;
pub const DENSITY_WEIGHT: f64 = 0.05;
pub const MIN_SAMPLE: u64 = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreInput {
    pub health: f64,
    pub latency_inverse: Option<f64>,
    pub reliability: Option<f64>,
    pub cost_inverse: Option<f64>,
    pub task_fit: Option<f64>,
    pub connection_density: f64,
    pub sample_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreBreakdown {
    pub health: f64,
    pub latency_inverse: f64,
    pub reliability: f64,
    pub cost_inverse: f64,
    pub task_fit: f64,
    pub connection_density: f64,
    pub total: f64,
}

fn bounded(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

pub fn normalize_metric(values: &[Option<f64>], inverse: bool) -> Vec<f64> {
    let known = values.iter().flatten().copied().collect::<Vec<_>>();
    if known.is_empty() {
        return vec![0.5; values.len()];
    }
    let min = known.iter().copied().fold(f64::INFINITY, f64::min);
    let max = known.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() || !max.is_finite() || (max - min).abs() < f64::EPSILON {
        return vec![0.5; values.len()];
    }
    values
        .iter()
        .map(|value| {
            value.map_or(0.5, |value| {
                let normalized = (value - min) / (max - min);
                bounded(if inverse {
                    1.0 - normalized
                } else {
                    normalized
                })
            })
        })
        .collect()
}

pub fn score(input: &ScoreInput) -> ScoreBreakdown {
    let sampled = input.sample_size >= MIN_SAMPLE;
    let health = bounded(input.health);
    let latency = if sampled {
        bounded(input.latency_inverse.unwrap_or(0.5))
    } else {
        0.5
    };
    let reliability = if sampled {
        bounded(input.reliability.unwrap_or(0.5))
    } else {
        0.5
    };
    let cost = bounded(input.cost_inverse.unwrap_or(0.5));
    let task_fit = bounded(input.task_fit.unwrap_or(0.5));
    let density = bounded(input.connection_density);
    ScoreBreakdown {
        health,
        latency_inverse: latency,
        reliability,
        cost_inverse: cost,
        task_fit,
        connection_density: density,
        total: health * HEALTH_WEIGHT
            + latency * LATENCY_WEIGHT
            + reliability * RELIABILITY_WEIGHT
            + cost * COST_WEIGHT
            + task_fit * TASK_FIT_WEIGHT
            + density * DENSITY_WEIGHT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_sum_to_one() {
        let total = HEALTH_WEIGHT
            + LATENCY_WEIGHT
            + RELIABILITY_WEIGHT
            + COST_WEIGHT
            + TASK_FIT_WEIGHT
            + DENSITY_WEIGHT;
        assert!((total - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn insufficient_data_is_neutral() {
        let result = score(&ScoreInput {
            health: 1.0,
            latency_inverse: Some(1.0),
            reliability: Some(1.0),
            cost_inverse: None,
            task_fit: None,
            connection_density: 1.0,
            sample_size: 2,
        });
        assert_eq!(result.latency_inverse, 0.5);
        assert_eq!(result.reliability, 0.5);
        assert_eq!(result.cost_inverse, 0.5);
    }

    #[test]
    fn normalization_is_neutral_without_variance_and_preserves_unknowns() {
        assert_eq!(
            normalize_metric(&[Some(10.0), Some(10.0), None], true),
            vec![0.5, 0.5, 0.5]
        );
        assert_eq!(
            normalize_metric(&[Some(10.0), Some(20.0), None], true),
            vec![1.0, 0.0, 0.5]
        );
    }
}
