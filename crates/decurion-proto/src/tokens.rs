//! What agents read and wrote, in tokens: for one run, one piece of work or a
//! whole goal. The board shows it so the owner can see how much of a Max
//! plan's allowance a goal used, and how much of that Claude read back from
//! its prompt cache instead of reading afresh.
//!
//! The runner fills a run's figures from what the agent reported; the hub
//! adds runs up per piece, per goal and per app. Every figure is a count the
//! agent gave, never a guess; a run that reported nothing adds nothing.

use serde::{Deserialize, Serialize};

/// Tokens for one run, piece or goal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUse {
    /// Input read afresh.
    #[serde(default)]
    pub fresh: u64,
    /// Input read back from the prompt cache: the same instructions and
    /// conversation, sent again on the next turn. It costs about a tenth of
    /// fresh input.
    #[serde(default)]
    pub cache_read: u64,
    /// Input written to the prompt cache so later turns can read it back.
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub output: u64,
    /// The fullest the agent's conversation got: the input of its biggest
    /// single turn. Claude Code compacts (summarises) the conversation when
    /// it nears its limit.
    #[serde(default)]
    pub peak_context: u64,
    /// How many times Claude Code compacted the conversation.
    #[serde(default)]
    pub compactions: u64,
}

impl TokenUse {
    /// All input, however it was read.
    pub fn input(&self) -> u64 {
        self.fresh
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    pub fn total(&self) -> u64 {
        self.input().saturating_add(self.output)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// The share of input read back from the cache, 0 to 100.
    pub fn cache_pct(&self) -> Option<u64> {
        let input = self.input();
        (input > 0).then(|| (self.cache_read as u128 * 100 / input as u128) as u64)
    }

    /// Adds another run's figures: counts add up, the peak is the larger.
    pub fn add(&mut self, o: &TokenUse) {
        self.fresh = self.fresh.saturating_add(o.fresh);
        self.cache_read = self.cache_read.saturating_add(o.cache_read);
        self.cache_write = self.cache_write.saturating_add(o.cache_write);
        self.output = self.output.saturating_add(o.output);
        self.peak_context = self.peak_context.max(o.peak_context);
        self.compactions = self.compactions.saturating_add(o.compactions);
    }

    /// A few plain words for the board: "1.2M tokens · 94% cached".
    pub fn words(&self) -> String {
        let mut s = format!("{} tokens", short(self.total()));
        if let Some(p) = self.cache_pct() {
            s.push_str(&format!(" · {p}% cached"));
        }
        s
    }
}

/// A token count the way people say it: 950, 12k, 1.2M.
pub fn short(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=99_999 => trim(format!("{:.1}", n as f64 / 1e3)) + "k",
        100_000..=999_999 => format!("{}k", n / 1_000),
        _ => trim(format!("{:.1}", n as f64 / 1e6)) + "M",
    }
}

fn trim(s: String) -> String {
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

/// A total with its words, as the board shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenLine {
    #[serde(flatten)]
    pub tokens: TokenUse,
    pub words: String,
}

impl From<TokenUse> for TokenLine {
    fn from(tokens: TokenUse) -> Self {
        TokenLine {
            words: tokens.words(),
            tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_the_way_people_say_them() {
        assert_eq!(short(950), "950");
        assert_eq!(short(1_000), "1k");
        assert_eq!(short(12_340), "12.3k");
        assert_eq!(short(187_000), "187k");
        assert_eq!(short(1_200_000), "1.2M");
        assert_eq!(short(3_000_000), "3M");
    }

    #[test]
    fn the_cache_share_is_of_all_input() {
        let t = TokenUse {
            fresh: 50,
            cache_read: 900,
            cache_write: 50,
            output: 10,
            ..TokenUse::default()
        };
        assert_eq!(t.input(), 1_000);
        assert_eq!(t.cache_pct(), Some(90));
        assert_eq!(t.words(), "1k tokens · 90% cached");
        assert_eq!(TokenUse::default().cache_pct(), None);
        assert_eq!(TokenUse::default().words(), "0 tokens");
    }

    #[test]
    fn adding_keeps_the_biggest_context_and_sums_the_rest() {
        let mut a = TokenUse {
            fresh: 1,
            peak_context: 100,
            compactions: 1,
            ..TokenUse::default()
        };
        a.add(&TokenUse {
            fresh: 2,
            peak_context: 40,
            compactions: 2,
            ..TokenUse::default()
        });
        assert_eq!((a.fresh, a.peak_context, a.compactions), (3, 100, 3));
    }
}
