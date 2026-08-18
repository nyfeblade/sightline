//! Rate card used to estimate what a session's tokens would cost at
//! first-party API prices. Claude Code subscription usage is not billed this
//! way — the figure is an equivalent, which is why the UI labels it "est".

pub struct Rates {
    /// dollars per million input tokens
    pub input: f64,
    /// dollars per million output tokens
    pub output: f64,
}

/// Cache reads bill at 0.1x the base input rate; writes at 1.25x (5m TTL) or
/// 2x (1h TTL).
pub const CACHE_READ: f64 = 0.1;
pub const CACHE_WRITE_5M: f64 = 1.25;
pub const CACHE_WRITE_1H: f64 = 2.0;

/// Rates for a model id as it appears in the transcript. Ids may carry a
/// context suffix (`claude-opus-5[1m]`), so match on the prefix. `fast` is
/// taken from `usage.speed` — fast mode is priced at a premium on the two
/// models that offer it.
pub fn rates(model: &str, fast: bool) -> Option<Rates> {
    let (input, output) = if model.starts_with("claude-fable") || model.starts_with("claude-mythos")
    {
        (10.0, 50.0)
    } else if model.starts_with("claude-opus-5") || model.starts_with("claude-opus-4-8") {
        if fast { (10.0, 50.0) } else { (5.0, 25.0) }
    } else if model.starts_with("claude-opus") {
        (5.0, 25.0)
    } else if model.starts_with("claude-sonnet") {
        (3.0, 15.0)
    } else if model.starts_with("claude-haiku") {
        (1.0, 5.0)
    } else {
        return None;
    };
    Some(Rates { input, output })
}
