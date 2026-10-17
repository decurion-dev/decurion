//! What a Messages API call costs, from the usage the API reports.
//!
//! Prices are Anthropic first-party rates in USD per million tokens, which is
//! the same number as micro-dollars per token. Cache writes cost 1.25x the
//! input price (5-minute) or 2x (1-hour); cache reads cost the per-model read
//! price. A model not in the table is priced at the most expensive model's
//! rates, so an unknown name can only over-count.
//!
//! Prices as of 2026-10; update this table when Anthropic's pricing changes.

/// Per-million-token prices, in USD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
}

const FABLE: Price = Price {
    input: 10.0,
    output: 50.0,
    cache_read: 0.25,
};

/// The most expensive rates any Claude model has had (Opus 4 / 4.1), used
/// for anything not recognised. Cache reads are priced as plain input to stay
/// on the high side.
const UNKNOWN: Price = Price {
    input: 15.0,
    output: 75.0,
    cache_read: 15.0,
};

const OPUS_4_6_TO_4_8: Price = Price {
    input: 5.0,
    output: 25.0,
    cache_read: 0.50,
};

/// Rates by model id. Longer, more specific prefixes come first.
const TABLE: &[(&str, Price)] = &[
    ("claude-fable-5-1", FABLE),
    ("claude-mythos-5-1", FABLE),
    (
        "claude-fable-5",
        Price {
            input: 10.0,
            output: 50.0,
            cache_read: 1.0,
        },
    ),
    (
        "claude-mythos-5",
        Price {
            input: 10.0,
            output: 50.0,
            cache_read: 1.0,
        },
    ),
    (
        "claude-opus-5-5",
        Price {
            input: 4.0,
            output: 20.0,
            cache_read: 0.20,
        },
    ),
    (
        "claude-opus-5",
        Price {
            input: 5.0,
            output: 25.0,
            cache_read: 0.50,
        },
    ),
    ("claude-opus-4-8", OPUS_4_6_TO_4_8),
    ("claude-opus-4-7", OPUS_4_6_TO_4_8),
    ("claude-opus-4-6", OPUS_4_6_TO_4_8),
    (
        "claude-sonnet-5",
        Price {
            input: 2.0,
            output: 10.0,
            cache_read: 0.20,
        },
    ),
    (
        "claude-sonnet-4-6",
        Price {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
        },
    ),
    (
        "claude-haiku-5-5",
        Price {
            input: 0.10,
            output: 0.50,
            cache_read: 0.01,
        },
    ),
    (
        "claude-haiku-4-5",
        Price {
            input: 1.0,
            output: 5.0,
            cache_read: 0.10,
        },
    ),
];

/// Haiku 5.5 charges more once a prompt goes over 100K tokens.
const HAIKU_55_LONG: Price = Price {
    input: 0.50,
    output: 2.50,
    cache_read: 0.05,
};

/// The rates for `model` (a dated or `-fast` suffix is fine).
pub fn price(model: &str) -> Price {
    TABLE
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map_or(UNKNOWN, |(_, p)| *p)
}

/// Tokens one call used, as the API reports them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    /// Fast mode costs twice the standard rate.
    pub fast: bool,
    pub web_searches: u64,
}

/// Web search is billed per search: $10 per 1,000.
const WEB_SEARCH_MICROS: u64 = 10_000;

/// What `usage` on `model` costs, in micro-dollars, rounded up.
pub fn cost_micros(model: &str, u: &Usage) -> u64 {
    let prompt = u.input + u.cache_read + u.cache_write_5m + u.cache_write_1h;
    let p = if model.starts_with("claude-haiku-5-5") && prompt > 100_000 {
        HAIKU_55_LONG
    } else {
        price(model)
    };
    let speed = if u.fast { 2.0 } else { 1.0 };
    let tokens = u.input as f64 * p.input
        + u.output as f64 * p.output
        + u.cache_read as f64 * p.cache_read
        + u.cache_write_5m as f64 * p.input * 1.25
        + u.cache_write_1h as f64 * p.input * 2.0;
    (tokens * speed).ceil() as u64 + u.web_searches * WEB_SEARCH_MICROS
}

/// Reads one `usage` object (a response's, or a stream event's) into `u`.
/// Stream events repeat cumulative counts, so each field keeps its largest
/// value.
pub fn read_usage(v: &serde_json::Value, u: &mut Usage) {
    let n = |k: &str| v[k].as_u64();
    let keep = |slot: &mut u64, x: Option<u64>| {
        if let Some(x) = x {
            *slot = (*slot).max(x);
        }
    };
    keep(&mut u.input, n("input_tokens"));
    keep(&mut u.output, n("output_tokens"));
    keep(&mut u.cache_read, n("cache_read_input_tokens"));
    let split = &v["cache_creation"];
    if split.is_object() {
        keep(
            &mut u.cache_write_5m,
            split["ephemeral_5m_input_tokens"].as_u64(),
        );
        keep(
            &mut u.cache_write_1h,
            split["ephemeral_1h_input_tokens"].as_u64(),
        );
    } else {
        // Without the split, count writes at the dearer 1-hour rate.
        keep(&mut u.cache_write_1h, n("cache_creation_input_tokens"));
    }
    keep(
        &mut u.web_searches,
        v["server_tool_use"]["web_search_requests"].as_u64(),
    );
    if v["speed"] == "fast" {
        u.fast = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_use_their_rates_and_unknown_ones_the_highest() {
        assert_eq!(price("claude-opus-5-5").input, 4.0);
        assert_eq!(price("claude-sonnet-5-5").output, 10.0);
        assert_eq!(price("claude-opus-4-8").input, 5.0);
        assert_eq!(price("claude-fable-5-1").cache_read, 0.25);
        assert_eq!(price("claude-fable-5").cache_read, 1.0);
        assert_eq!(price("claude-something-new"), UNKNOWN);
        assert_eq!(price("claude-opus-4-1"), UNKNOWN);
    }

    #[test]
    fn a_call_is_priced_from_every_kind_of_token() {
        let u = Usage {
            input: 1_000,
            output: 500,
            cache_read: 10_000,
            cache_write_5m: 2_000,
            cache_write_1h: 0,
            fast: false,
            web_searches: 1,
        };
        // Opus 5.5: 1000*4 + 500*20 + 10000*0.2 + 2000*4*1.25 + one search.
        assert_eq!(
            cost_micros("claude-opus-5-5", &u),
            4_000 + 10_000 + 2_000 + 10_000 + 10_000
        );
        let fast = Usage {
            fast: true,
            web_searches: 0,
            ..u
        };
        assert_eq!(
            cost_micros("claude-opus-5-5", &fast),
            2 * (4_000 + 10_000 + 2_000 + 10_000)
        );
    }

    #[test]
    fn long_haiku_prompts_cost_more() {
        let short = Usage {
            input: 50_000,
            ..Default::default()
        };
        let long = Usage {
            input: 150_000,
            ..Default::default()
        };
        assert_eq!(cost_micros("claude-haiku-5-5", &short), 5_000);
        assert_eq!(cost_micros("claude-haiku-5-5", &long), 75_000);
    }

    #[test]
    fn stream_usage_keeps_the_largest_counts() {
        let mut u = Usage::default();
        read_usage(
            &serde_json::json!({"input_tokens": 12, "output_tokens": 1,
                "cache_creation": {"ephemeral_5m_input_tokens": 30, "ephemeral_1h_input_tokens": 0},
                "cache_read_input_tokens": 400}),
            &mut u,
        );
        read_usage(&serde_json::json!({"output_tokens": 250}), &mut u);
        read_usage(&serde_json::json!({"output_tokens": 240}), &mut u);
        assert_eq!(
            u,
            Usage {
                input: 12,
                output: 250,
                cache_read: 400,
                cache_write_5m: 30,
                ..Default::default()
            }
        );
    }
}
