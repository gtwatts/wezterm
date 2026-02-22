//! Multi-provider model routing with cost tracking.
//!
//! Manages a list of configured models, supports switching between them,
//! and tracks token usage and cost per model.

use std::collections::HashMap;

/// Configuration for a single model entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelConfig {
    /// Model identifier (e.g., "gemini-2.5-pro").
    pub name: String,
    /// Provider name (e.g., "gemini", "anthropic").
    pub provider: String,
    /// Human-friendly display name (e.g., "Gemini Pro").
    #[serde(default)]
    pub display_name: String,
    /// Whether this is the default model.
    #[serde(default)]
    pub default: bool,
    /// Cost per 1K input tokens in USD (optional override).
    #[serde(default)]
    pub cost_per_1k_input: f64,
    /// Cost per 1K output tokens in USD (optional override).
    #[serde(default)]
    pub cost_per_1k_output: f64,
}

impl ModelConfig {
    /// The name to show in the UI (falls back to model name).
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }
}

/// Tracks cumulative token usage and cost.
#[derive(Debug, Clone, Default)]
pub struct CostTracker {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    /// Per-model breakdown: model_name -> (input_tokens, output_tokens, cost_usd).
    pub per_model: HashMap<String, (u64, u64, f64)>,
}

impl CostTracker {
    /// Record token usage for a model.
    pub fn record(
        &mut self,
        model_name: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_per_1k_input: f64,
        cost_per_1k_output: f64,
    ) {
        self.total_input_tokens += input_tokens;
        self.total_output_tokens += output_tokens;

        let cost = (input_tokens as f64 / 1000.0) * cost_per_1k_input
            + (output_tokens as f64 / 1000.0) * cost_per_1k_output;
        self.total_cost_usd += cost;

        let entry = self
            .per_model
            .entry(model_name.to_string())
            .or_insert((0, 0, 0.0));
        entry.0 += input_tokens;
        entry.1 += output_tokens;
        entry.2 += cost;
    }

    /// Format total cost as a short string (e.g., "$0.042").
    pub fn format_cost(&self) -> String {
        if self.total_cost_usd < 0.001 {
            format!("${:.4}", self.total_cost_usd)
        } else if self.total_cost_usd < 1.0 {
            format!("${:.3}", self.total_cost_usd)
        } else {
            format!("${:.2}", self.total_cost_usd)
        }
    }
}

/// Routes between configured models and tracks usage.
#[derive(Debug, Clone)]
pub struct ModelRouter {
    models: Vec<ModelConfig>,
    active_index: usize,
    pub cost_tracker: CostTracker,
}

impl ModelRouter {
    /// Create a router from a list of model configs.
    ///
    /// The active model defaults to the first entry marked `default = true`,
    /// or the first entry if none is marked.
    pub fn new(models: Vec<ModelConfig>) -> Self {
        let active_index = models
            .iter()
            .position(|m| m.default)
            .unwrap_or(0);
        Self {
            models,
            active_index,
            cost_tracker: CostTracker::default(),
        }
    }

    /// Create a router from the legacy single-provider config fields.
    pub fn from_single(provider: &str, model: &str) -> Self {
        let config = ModelConfig {
            name: model.to_string(),
            provider: provider.to_string(),
            display_name: String::new(),
            default: true,
            cost_per_1k_input: 0.0,
            cost_per_1k_output: 0.0,
        };
        Self::new(vec![config])
    }

    /// The currently active model config.
    pub fn active_model(&self) -> &ModelConfig {
        &self.models[self.active_index]
    }

    /// All configured models.
    pub fn models(&self) -> &[ModelConfig] {
        &self.models
    }

    /// Number of configured models.
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// Cycle to the next model in the list (wraps around).
    pub fn cycle_model(&mut self) -> &ModelConfig {
        if self.models.len() > 1 {
            self.active_index = (self.active_index + 1) % self.models.len();
        }
        &self.models[self.active_index]
    }

    /// Switch to the model with the given name.
    ///
    /// Returns `true` if the model was found and switched to.
    pub fn switch_to(&mut self, name: &str) -> bool {
        let name_lower = name.to_lowercase();
        if let Some(idx) = self
            .models
            .iter()
            .position(|m| m.name.to_lowercase() == name_lower)
        {
            self.active_index = idx;
            true
        } else {
            false
        }
    }

    /// Record token usage for the currently active model.
    pub fn record_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        let model = &self.models[self.active_index];
        let (input_cost, output_cost) = if model.cost_per_1k_input > 0.0
            || model.cost_per_1k_output > 0.0
        {
            (model.cost_per_1k_input, model.cost_per_1k_output)
        } else {
            // Try to look up pricing from elwood-core's pricing table
            elwood_core::provider::lookup_pricing(&model.name)
                .map(|p| (p.input_per_1k, p.output_per_1k))
                .unwrap_or((0.0, 0.0))
        };

        self.cost_tracker
            .record(&model.name, input_tokens, output_tokens, input_cost, output_cost);
    }

    /// Format a short list of models for display (e.g., in `/model list`).
    pub fn format_model_list(&self) -> String {
        let mut out = String::new();
        for (i, model) in self.models.iter().enumerate() {
            let marker = if i == self.active_index { "* " } else { "  " };
            let label = model.label();
            let provider = &model.provider;
            out.push_str(&format!("{marker}{label} ({provider})"));
            if model.default {
                out.push_str(" [default]");
            }
            out.push('\n');
        }
        if self.cost_tracker.total_cost_usd > 0.0 {
            out.push_str(&format!(
                "\nSession cost: {} ({} input + {} output tokens)\n",
                self.cost_tracker.format_cost(),
                self.cost_tracker.total_input_tokens,
                self.cost_tracker.total_output_tokens,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_models() -> Vec<ModelConfig> {
        vec![
            ModelConfig {
                name: "gemini-2.5-pro".into(),
                provider: "gemini".into(),
                display_name: "Gemini Pro".into(),
                default: true,
                cost_per_1k_input: 0.00125,
                cost_per_1k_output: 0.005,
            },
            ModelConfig {
                name: "gemini-2.5-flash".into(),
                provider: "gemini".into(),
                display_name: String::new(),
                default: false,
                cost_per_1k_input: 0.000075,
                cost_per_1k_output: 0.0003,
            },
            ModelConfig {
                name: "claude-sonnet-4-6".into(),
                provider: "anthropic".into(),
                display_name: "Claude Sonnet".into(),
                default: false,
                cost_per_1k_input: 0.003,
                cost_per_1k_output: 0.015,
            },
        ]
    }

    #[test]
    fn test_new_selects_default() {
        let router = ModelRouter::new(sample_models());
        assert_eq!(router.active_model().name, "gemini-2.5-pro");
    }

    #[test]
    fn test_new_no_default_selects_first() {
        let models = vec![
            ModelConfig {
                name: "a".into(),
                provider: "x".into(),
                display_name: String::new(),
                default: false,
                cost_per_1k_input: 0.0,
                cost_per_1k_output: 0.0,
            },
            ModelConfig {
                name: "b".into(),
                provider: "x".into(),
                display_name: String::new(),
                default: false,
                cost_per_1k_input: 0.0,
                cost_per_1k_output: 0.0,
            },
        ];
        let router = ModelRouter::new(models);
        assert_eq!(router.active_model().name, "a");
    }

    #[test]
    fn test_cycle_model() {
        let mut router = ModelRouter::new(sample_models());
        assert_eq!(router.active_model().name, "gemini-2.5-pro");
        router.cycle_model();
        assert_eq!(router.active_model().name, "gemini-2.5-flash");
        router.cycle_model();
        assert_eq!(router.active_model().name, "claude-sonnet-4-6");
        router.cycle_model();
        assert_eq!(router.active_model().name, "gemini-2.5-pro"); // wraps
    }

    #[test]
    fn test_switch_to() {
        let mut router = ModelRouter::new(sample_models());
        assert!(router.switch_to("claude-sonnet-4-6"));
        assert_eq!(router.active_model().name, "claude-sonnet-4-6");
        assert!(!router.switch_to("nonexistent"));
        assert_eq!(router.active_model().name, "claude-sonnet-4-6"); // unchanged
    }

    #[test]
    fn test_switch_to_case_insensitive() {
        let mut router = ModelRouter::new(sample_models());
        assert!(router.switch_to("Claude-Sonnet-4-6"));
        assert_eq!(router.active_model().name, "claude-sonnet-4-6");
    }

    #[test]
    fn test_record_usage_and_cost() {
        let mut router = ModelRouter::new(sample_models());
        router.record_usage(1000, 500);

        assert_eq!(router.cost_tracker.total_input_tokens, 1000);
        assert_eq!(router.cost_tracker.total_output_tokens, 500);

        // cost = (1000/1000 * 0.00125) + (500/1000 * 0.005) = 0.00125 + 0.0025 = 0.00375
        let expected = 0.00375;
        assert!((router.cost_tracker.total_cost_usd - expected).abs() < 1e-9);

        let per_model = router.cost_tracker.per_model.get("gemini-2.5-pro").unwrap();
        assert_eq!(per_model.0, 1000);
        assert_eq!(per_model.1, 500);
    }

    #[test]
    fn test_format_cost() {
        let mut tracker = CostTracker::default();
        assert_eq!(tracker.format_cost(), "$0.0000");

        tracker.total_cost_usd = 0.042;
        assert_eq!(tracker.format_cost(), "$0.042");

        tracker.total_cost_usd = 1.5;
        assert_eq!(tracker.format_cost(), "$1.50");
    }

    #[test]
    fn test_from_single() {
        let router = ModelRouter::from_single("gemini", "gemini-2.5-pro");
        assert_eq!(router.model_count(), 1);
        assert_eq!(router.active_model().name, "gemini-2.5-pro");
        assert_eq!(router.active_model().provider, "gemini");
    }

    #[test]
    fn test_cycle_single_model_noop() {
        let mut router = ModelRouter::from_single("gemini", "gemini-2.5-pro");
        router.cycle_model();
        assert_eq!(router.active_model().name, "gemini-2.5-pro");
    }

    #[test]
    fn test_label_fallback() {
        let m = ModelConfig {
            name: "test-model".into(),
            provider: "test".into(),
            display_name: String::new(),
            default: false,
            cost_per_1k_input: 0.0,
            cost_per_1k_output: 0.0,
        };
        assert_eq!(m.label(), "test-model");

        let m2 = ModelConfig {
            name: "test-model".into(),
            provider: "test".into(),
            display_name: "Pretty Name".into(),
            default: false,
            cost_per_1k_input: 0.0,
            cost_per_1k_output: 0.0,
        };
        assert_eq!(m2.label(), "Pretty Name");
    }

    #[test]
    fn test_format_model_list() {
        let router = ModelRouter::new(sample_models());
        let list = router.format_model_list();
        assert!(list.contains("* Gemini Pro (gemini)"));
        assert!(list.contains("  gemini-2.5-flash (gemini)"));
        assert!(list.contains("  Claude Sonnet (anthropic)"));
        assert!(list.contains("[default]"));
    }
}
