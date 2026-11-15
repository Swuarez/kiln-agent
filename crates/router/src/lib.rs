//! Turns "coding" into an ordered list of provider+model pairs.
//!
//! Health is observed, not assumed: a 429 cools a target down, repeated failures
//! push it behind its peers, and observed latency decides `auto` order. This is
//! the part that makes free-tier or trial providers usable without babysitting.

use kiln_core::config::{LogicalModel, Strategy, Target};
use kiln_core::{Error, Result};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_LATENCY_MS: f64 = 800.0;
const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(30);
const MIN_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_COOLDOWN: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct Health {
    pub successes: u32,
    pub failures: u32,
    pub rate_limits: u32,
    /// Exponential moving average of successful round-trip time.
    pub latency_ms: f64,
    pub cooldown_until: Option<Instant>,
    pub last_error: Option<String>,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            successes: 0,
            failures: 0,
            rate_limits: 0,
            latency_ms: DEFAULT_LATENCY_MS,
            cooldown_until: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Failure {
    /// 429, with the provider's Retry-After when it told us.
    RateLimited(Option<Duration>),
    Auth,
    Network,
    Server,
    /// 400-class: our request was wrong, not the provider. No cooldown.
    Client,
}

#[derive(Debug, Clone)]
pub struct TargetHealth {
    pub provider: String,
    pub model: String,
    pub health: Health,
}

pub struct Router {
    models: BTreeMap<String, LogicalModel>,
    health: Mutex<HashMap<String, Health>>,
    cursors: Mutex<HashMap<String, usize>>,
}

fn key(target: &Target) -> String {
    format!("{}/{}", target.provider, target.model)
}

impl Router {
    pub fn new(models: BTreeMap<String, LogicalModel>) -> Self {
        Self {
            models,
            health: Mutex::new(HashMap::new()),
            cursors: Mutex::new(HashMap::new()),
        }
    }

    /// Targets in the order they should be attempted.
    pub fn order(&self, name: &str) -> Result<Vec<Target>> {
        let model = self
            .models
            .get(name)
            .ok_or_else(|| Error::config(format!("unknown model '{name}'")))?;
        let now = Instant::now();
        let health = self.health.lock().expect("health lock");

        let is_available = |t: &Target| {
            health
                .get(&key(t))
                .and_then(|h| h.cooldown_until)
                .map_or(true, |until| until <= now)
        };
        let score = |t: &Target| {
            let h = health.get(&key(t));
            let base = h.map_or(DEFAULT_LATENCY_MS, |h| h.latency_ms);
            let (failures, limits) = h.map_or((0, 0), |h| (h.failures, h.rate_limits));
            base + f64::from(failures) * 500.0 + f64::from(limits) * 1500.0
        };

        let mut ordered: Vec<Target> = match model.strategy {
            Strategy::Priority => model.targets.clone(),
            Strategy::RoundRobin => {
                let mut expanded: Vec<Target> = Vec::new();
                for t in &model.targets {
                    for _ in 0..t.weight.min(100) {
                        expanded.push(t.clone());
                    }
                }
                if expanded.is_empty() {
                    return Ok(Vec::new());
                }
                let mut cursor = self.cursors.lock().expect("cursor lock");
                let start = *cursor.entry(name.to_string()).or_insert(0) % expanded.len();
                *cursor.get_mut(name).expect("cursor exists") = start + 1;
                expanded.rotate_left(start);
                expanded
            }
            Strategy::Auto => {
                let mut targets = model.targets.clone();
                targets.sort_by(|a, b| score(a).total_cmp(&score(b)));
                targets
            }
        };

        // Cooling targets go last but are kept: if everything is limited, trying
        // the least recently limited is still better than failing outright.
        ordered.sort_by_key(|t| !is_available(t));
        Ok(ordered)
    }

    pub fn record_success(&self, target: &Target, latency: Duration) {
        let mut health = self.health.lock().expect("health lock");
        let entry = health.entry(key(target)).or_default();
        let ms = latency.as_secs_f64() * 1000.0;
        entry.latency_ms = if entry.successes == 0 {
            ms
        } else {
            entry.latency_ms * 0.7 + ms * 0.3
        };
        entry.successes += 1;
        entry.cooldown_until = None;
        entry.last_error = None;
    }

    pub fn record_failure(&self, target: &Target, failure: Failure) {
        let cooldown = match failure {
            Failure::RateLimited(retry) => Some(
                retry
                    .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN)
                    .clamp(MIN_COOLDOWN, MAX_COOLDOWN),
            ),
            Failure::Auth => Some(Duration::from_secs(600)),
            Failure::Network => Some(Duration::from_secs(10)),
            Failure::Server => Some(Duration::from_secs(15)),
            Failure::Client => None,
        };
        let mut health = self.health.lock().expect("health lock");
        let entry = health.entry(key(target)).or_default();
        entry.failures += 1;
        if matches!(failure, Failure::RateLimited(_)) {
            entry.rate_limits += 1;
        }
        entry.last_error = Some(format!("{failure:?}"));
        if let Some(cd) = cooldown {
            entry.cooldown_until = Some(Instant::now() + cd);
        }
    }

    /// Snapshot for `kiln models --verbose` and trace logs.
    pub fn health_snapshot(&self) -> Vec<TargetHealth> {
        let health = self.health.lock().expect("health lock");
        let mut out: Vec<TargetHealth> = self
            .models
            .values()
            .flat_map(|m| m.targets.iter())
            .map(|t| TargetHealth {
                provider: t.provider.clone(),
                model: t.model.clone(),
                health: health.get(&key(t)).cloned().unwrap_or_default(),
            })
            .collect();
        out.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then_with(|| a.model.cmp(&b.model))
        });
        out.dedup_by(|a, b| a.provider == b.provider && a.model == b.model);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(provider: &str, model: &str) -> Target {
        Target {
            provider: provider.into(),
            model: model.into(),
            weight: 1,
        }
    }

    fn router(strategy: Strategy, targets: Vec<Target>) -> Router {
        let mut models = BTreeMap::new();
        models.insert("m".to_string(), LogicalModel { strategy, targets });
        Router::new(models)
    }

    fn providers(order: &[Target]) -> Vec<&str> {
        order.iter().map(|t| t.provider.as_str()).collect()
    }

    #[test]
    fn priority_keeps_configured_order() {
        let r = router(Strategy::Priority, vec![target("a", "x"), target("b", "y")]);
        assert_eq!(providers(&r.order("m").unwrap()), ["a", "b"]);
    }

    #[test]
    fn rate_limited_target_moves_to_the_back() {
        let r = router(Strategy::Priority, vec![target("a", "x"), target("b", "y")]);
        r.record_failure(
            &target("a", "x"),
            Failure::RateLimited(Some(Duration::from_secs(60))),
        );
        assert_eq!(providers(&r.order("m").unwrap()), ["b", "a"]);
    }

    #[test]
    fn auto_prefers_the_faster_target() {
        let r = router(
            Strategy::Auto,
            vec![target("slow", "x"), target("fast", "y")],
        );
        r.record_success(&target("slow", "x"), Duration::from_millis(4000));
        r.record_success(&target("fast", "y"), Duration::from_millis(300));
        assert_eq!(providers(&r.order("m").unwrap()), ["fast", "slow"]);
    }

    #[test]
    fn round_robin_rotates_and_respects_weights() {
        let mut weighted = target("b", "y");
        weighted.weight = 2;
        let r = router(Strategy::RoundRobin, vec![target("a", "x"), weighted]);
        let o1 = r.order("m").unwrap();
        let o2 = r.order("m").unwrap();
        let o3 = r.order("m").unwrap();
        let o4 = r.order("m").unwrap();
        let first = providers(&o1);
        let second = providers(&o2);
        let third = providers(&o3);
        let fourth = providers(&o4);
        assert_ne!(first[0], second[0], "rotation should change the leader");
        assert_eq!(third[0], "b", "weighted target leads twice: {:?}", third);
        assert_eq!(fourth, first, "rotation returns to the start after 3 slots");
        assert_eq!(first.len(), 3, "weight 2 expands to two slots");
    }

    #[test]
    fn unknown_model_is_an_error() {
        let r = router(Strategy::Priority, vec![target("a", "x")]);
        assert!(r.order("nope").is_err());
    }

    #[test]
    fn success_clears_a_cooldown() {
        let r = router(Strategy::Priority, vec![target("a", "x")]);
        r.record_failure(&target("a", "x"), Failure::Server);
        r.record_success(&target("a", "x"), Duration::from_millis(100));
        let snapshot = r.health_snapshot();
        assert!(snapshot[0].health.cooldown_until.is_none());
        assert_eq!(snapshot[0].health.successes, 1);
    }
}
