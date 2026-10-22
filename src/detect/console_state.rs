//! Pure-ANSI console-state detector for Tau/TauRlm panes.
//!
//! This module classifies Tau/TauRlm terminal state from visible ANSI content,
//! completely independent of A2A/lifecycle reporting. The detector is pure:
//! it takes snapshot strings and optional timing context, returns a classification.

use regex::Regex;
use std::sync::OnceLock;

use super::{Agent, AgentDetection, AgentState};

// ---------------------------------------------------------------------------
// Internal console states (richer than Herdr dot states)
// ---------------------------------------------------------------------------

/// Rich internal state derived from visible terminal content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleState {
    /// Prompt visible, stable after debounce -- agent finished.
    Finished,
    /// Fresh visible content after a non-prompt line (single-color output).
    WorkingSingle,
    /// Colored output delta (multi-color output, tool output, status bars).
    WorkingDouble,
    /// Quiet interval, no prompt visible -- model is thinking/streaming.
    Breathing,
    /// Visible blocked/waiting-for-input indicator.
    Blocked,
    /// Visible error indicator.
    Error,
    /// No sufficient evidence to classify.
    Unknown,
}

#[allow(dead_code)]
impl ConsoleState {
    /// Map internal state to Herdr dot AgentState.
    pub fn to_agent_state(self) -> AgentState {
        match self {
            Self::Finished => AgentState::Idle,
            Self::WorkingSingle | Self::WorkingDouble | Self::Breathing => AgentState::Working,
            Self::Blocked => AgentState::Blocked,
            Self::Error | Self::Unknown => AgentState::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Finished => "FINISHED",
            Self::WorkingSingle => "WORKING_SINGLE",
            Self::WorkingDouble => "WORKING_DOUBLE",
            Self::Breathing => "BREATHING",
            Self::Blocked => "BLOCKED",
            Self::Error => "ERROR",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// Full classification result with metadata for token reporting.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ConsoleClassification {
    pub state: ConsoleState,
    pub dot_state: AgentState,
    /// Human-readable detail for tau-console-detail token.
    pub detail: String,
    /// Detected prompt text for tau-console-prompt token.
    pub prompt: Option<String>,
}

// ---------------------------------------------------------------------------
// ANSI helpers
// ---------------------------------------------------------------------------

/// Strip ANSI escape sequences from text, preserving visible characters.
pub(crate) fn strip_ansi(text: &str) -> String {
    static ANSI_RE: OnceLock<Regex> = OnceLock::new();
    let re = ANSI_RE.get_or_init(|| {
        Regex::new(
            r"\x1b\[[0-9;?]*[a-zA-Z@`]|\x1b\][^\x07]*\x07|\x1b[P^_][^\x1b]*\x1b\\|\x1b[()][A-Z0-9]",
        )
        .unwrap()
    });
    re.replace_all(text, "").into_owned()
}

/// Check if a region of raw ANSI contains colored text (SGR codes that set
/// foreground/background colors other than default white/black).
pub(crate) fn has_color_activity(raw: &str) -> bool {
    static SGR_COLOR: OnceLock<Regex> = OnceLock::new();
    let re = SGR_COLOR.get_or_init(|| {
        Regex::new(r"\x1b\[(?:[0-9;]*3[0-7]|[0-9;]*9[0-7]|[0-9;]*4[0-7]|[0-9;]*10[0-7])m").unwrap()
    });
    re.is_match(raw)
}

/// Check if raw ANSI contains any SGR sequence (color or style change).
#[allow(dead_code)] // available for future console-state extensions
pub(crate) fn has_sgr_activity(raw: &str) -> bool {
    static SGR: OnceLock<Regex> = OnceLock::new();
    let re = SGR.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());
    re.is_match(raw)
}

// ---------------------------------------------------------------------------
// Prompt detection
// ---------------------------------------------------------------------------

/// Tau prompt: `>>>` at start of line or first non-space position.
/// TauRLM prompt: `rlm[#]>>>` or just `rlm...>>>`.
/// Suffix text after prompt is allowed (typing at prompt).
pub(crate) fn detect_prompt(lines: &[&str]) -> Option<(String, usize)> {
    static TAU_PROMPT: OnceLock<Regex> = OnceLock::new();
    static RLM_PROMPT: OnceLock<Regex> = OnceLock::new();

    let tau_re = TAU_PROMPT.get_or_init(|| Regex::new(r"^\s*>>>").unwrap());
    let rlm_re = RLM_PROMPT.get_or_init(|| Regex::new(r"^\s*rlm\[\d+\][^>]*>>>").unwrap());

    // Scan bottom 20 lines for a prompt
    let scan_start = lines.len().saturating_sub(20);
    for i in (scan_start..lines.len()).rev() {
        let line = lines[i].trim_end();
        if line.is_empty() {
            continue;
        }
        let stripped = strip_ansi(line);
        if rlm_re.is_match(&stripped) {
            return Some((stripped, i));
        }
        if tau_re.is_match(&stripped) {
            return Some((stripped, i));
        }
        // If a non-empty, non-prompt line, stop scanning further up
        break;
    }
    None
}

/// Check whether a prompt line has suffix text (user has typed something).
/// Suffix text at prompt should NOT count as working output.
pub(crate) fn prompt_has_suffix(prompt_line: &str) -> bool {
    static PROMPT_ONLY: OnceLock<Regex> = OnceLock::new();
    let re = PROMPT_ONLY
        .get_or_init(|| Regex::new(r"^\s*(?:rlm\[\d+\][^>]*>>>|>>>)[\s_\x1b\[\?]*$").unwrap());
    !re.is_match(prompt_line)
}

/// Whether the prompt suffix is an active status indicator (not typed input).
/// Tau sets `>>> [RUNNING] ` while turn_active; TauRLM sets
/// `rlm[N]>>> [RUNNING] `. Treat any bracketed uppercase activity word on the
/// prompt line as live working evidence, not typed input.
pub(crate) fn prompt_suffix_is_active_status(prompt_line: &str) -> bool {
    static ACTIVE_STATUS: OnceLock<Regex> = OnceLock::new();
    let re = ACTIVE_STATUS.get_or_init(|| {
        Regex::new(
            r"(?i)\[\s*(?:RUNNING|WORKING|WORKING_SINGLE|WORKING_DOUBLE|BREATHING|THINKING|STREAMING|GENERATING|BUSY|ACTING)\s*\]"
        ).unwrap()
    });
    re.is_match(prompt_line)
}

// ---------------------------------------------------------------------------
// Blocked indicators
// ---------------------------------------------------------------------------

pub(crate) fn detect_blocked(plain_lines: &[&str]) -> Option<String> {
    let scan_start = plain_lines.len().saturating_sub(20);
    let region: String = plain_lines[scan_start..]
        .to_vec()
        .join(" ");
    let lower = region.to_lowercase();

    static BLOCKED_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = BLOCKED_PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"\b(waiting|waiting\s+for)\s+(input|response|confirmation|permission)")
                .unwrap(),
            Regex::new(r"\bconfirm\s*\[y/n\]").unwrap(),
            Regex::new(r"\bpress\s+enter\s+to\s+(continue|proceed|return)").unwrap(),
            Regex::new(r"\bawaiting\s+(?:\w+\s+)?(input|response|confirmation)").unwrap(),
            Regex::new(r"\bpermission\s+(required|requested|needed)").unwrap(),
            Regex::new(r"\btype\s+y\s*(/|or)\s*n\b").unwrap(),
            Regex::new(r"(?im)^.*\[.*blocked.*\].*$").unwrap(),
            Regex::new(r"(?i)status.*waiting.*input").unwrap(),
            Regex::new(r"(?i)input\s+required").unwrap(),
            Regex::new(r"(?i)confirmation\s+required").unwrap(),
            Regex::new(r"(?i)\bdouble-call\s+confirmation\b").unwrap(),
            Regex::new(r"(?i)\bchoose\s+a\s+tool\b").unwrap(),
        ]
    });

    for re in patterns {
        if re.is_match(&lower) {
            return Some(re.as_str().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Error indicators
// ---------------------------------------------------------------------------

/// True if a raw ANSI screen contains visible foreground-red text.
///
/// For Tau/TauRlm console-state detection, red foreground text is the
/// authoritative ERROR signal. This ignores background red, inverse, and
/// red-only padding.
pub(crate) fn visible_red_text(raw_ansi: &str) -> bool {
    let bytes = raw_ansi.as_bytes();
    let mut idx = 0usize;
    let mut red = false;

    while idx < bytes.len() {
        let byte = bytes[idx];

        if byte == 0x1b && idx + 1 < bytes.len() {
            if bytes[idx + 1] == b'[' {
                let mut end = idx + 2;
                while end < bytes.len() {
                    let c = bytes[end];
                    if c.is_ascii_digit() || c == b';' || c == b':' || (0x3c..=0x3f).contains(&c) {
                        end += 1;
                    } else {
                        break;
                    }
                }
                if end < bytes.len() && bytes[end] == b'm' {
                    let params = &raw_ansi[idx + 2..end];
                    apply_sgr_params(params, &mut red);
                }
                idx = end + 1;
                continue;
            }
            idx += 1;
            continue;
        }

        if red && byte >= 0x21 && byte != 0x7f {
            return true;
        }

        idx += 1;
    }

    false
}

fn apply_sgr_params(params: &str, red: &mut bool) {
    let mut parts = params.split(';').filter(|p| !p.is_empty());
    while let Some(part) = parts.next() {
        let Ok(code) = part.parse::<u16>() else {
            continue;
        };

        match code {
            0 | 39 => *red = false,
            30..=37 => *red = code == 31,
            90..=97 => *red = code == 91,
            38 => match parts.next().and_then(|mode| mode.parse::<u16>().ok()) {
                Some(5) => {
                    if let Some(color) = parts.next().and_then(|color| color.parse::<u16>().ok()) {
                        *red = matches!(color, 1 | 9);
                    }
                }
                Some(2) => {
                    let r = parts.next().and_then(|value| value.parse::<u8>().ok());
                    let g = parts.next().and_then(|value| value.parse::<u8>().ok());
                    let b = parts.next().and_then(|value| value.parse::<u8>().ok());
                    if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                        *red = rgb_is_red(r, g, b);
                    }
                }
                _ => {}
            },
            48 => match parts.next().and_then(|mode| mode.parse::<u16>().ok()) {
                Some(5) => {
                    let _ = parts.next();
                }
                Some(2) => {
                    let _ = parts.next();
                    let _ = parts.next();
                    let _ = parts.next();
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn rgb_is_red(r: u8, g: u8, b: u8) -> bool {
    let rf = f32::from(r) / 255.0;
    let gf = f32::from(g) / 255.0;
    let bf = f32::from(b) / 255.0;
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let chroma = max - min;

    if max < 0.55 || chroma < 0.15 {
        return false;
    }

    let hue = if max == rf {
        60.0 * ((gf - bf) / chroma)
    } else if max == gf {
        60.0 * ((bf - rf) / chroma + 2.0)
    } else {
        60.0 * ((rf - gf) / chroma + 4.0)
    };
    let hue = if hue < 0.0 { hue + 360.0 } else { hue };
    let saturation = chroma / max;

    saturation >= 0.35 && (hue <= 15.0 || hue >= 345.0)
}

pub(crate) fn detect_error(raw_ansi: &str) -> Option<String> {
    if visible_red_text(raw_ansi) {
        Some("red_text".to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Content delta analysis
// ---------------------------------------------------------------------------

/// Detect if raw ANSI content contains fresh visible output beyond just
/// prompt/cursor/status-bar redraws.
pub(crate) fn content_is_fresh_output(plain: &str) -> bool {
    let stripped = strip_ansi(plain);
    let trimmed = stripped.trim();
    if trimmed.len() <= 2 {
        return false;
    }
    for line in trimmed.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // Status bar indicators -- bracketed single-word status
        if t.starts_with('[') && t.ends_with(']') && t.len() < 40 {
            continue;
        }
        // Progress spinner patterns
        if t.starts_with('.') || t == "..." || t == "...." {
            continue;
        }
        if t.len() > 5 {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Main classifier
// ---------------------------------------------------------------------------

/// Classify console state from a raw ANSI screen snapshot (bottom N lines).
///
/// * `raw_ansi` -- bottom N lines of terminal with ANSI escape codes intact.
/// * `quiet_age_secs` -- seconds since last PTY output (None = no timing data).
pub fn classify(raw_ansi: &str, quiet_age_secs: Option<f64>) -> ConsoleClassification {
    let plain = strip_ansi(raw_ansi);
    let plain_lines: Vec<&str> = plain.lines().collect();
    let ansi_lines: Vec<&str> = raw_ansi.lines().collect();

    let prompt = detect_prompt(&ansi_lines);
    let has_prompt = prompt.is_some();
    let prompt_line = prompt.as_ref().map(|(text, _)| text.clone());
    let prompt_at_bottom = prompt
        .as_ref()
        .map(|(_, idx)| *idx >= ansi_lines.len().saturating_sub(3))
        .unwrap_or(false);

    // 1. Error detection (highest priority after prompt)
    if let Some(pattern) = detect_error(raw_ansi) {
        return ConsoleClassification {
            state: ConsoleState::Error,
            dot_state: AgentState::Unknown,
            detail: pattern,
            prompt: prompt_line,
        };
    }

    // 2. Blocked detection
    if let Some(pattern) = detect_blocked(&plain_lines) {
        return ConsoleClassification {
            state: ConsoleState::Blocked,
            dot_state: AgentState::Blocked,
            detail: format!("blocked_pattern:{}", pattern),
            prompt: prompt_line,
        };
    }

    // 3. Prompt visible at bottom
    if has_prompt && prompt_at_bottom {
        let suffix = prompt_line
            .as_ref()
            .map(|p| prompt_has_suffix(p))
            .unwrap_or(false);
        let active_status = prompt_line
            .as_ref()
            .map(|p| prompt_suffix_is_active_status(p))
            .unwrap_or(false);

        if active_status {
            // Prompt carries an active status indicator (e.g. `[RUNNING]` while
            // the agent turn is active) => working, regardless of timing.
            let colored = has_color_activity(raw_ansi);
            return ConsoleClassification {
                state: if colored {
                    ConsoleState::WorkingDouble
                } else {
                    ConsoleState::WorkingSingle
                },
                dot_state: AgentState::Working,
                detail: "prompt_status_active".to_string(),
                prompt: prompt_line,
            };
        }

        if suffix {
            // Prompt with typed text = idle/finished
            return ConsoleClassification {
                state: ConsoleState::Finished,
                dot_state: AgentState::Idle,
                detail: "prompt_with_suffix".to_string(),
                prompt: prompt_line,
            };
        }

        // Bare prompt: use timing
        if let Some(quiet) = quiet_age_secs {
            if quiet > 1.5 {
                return ConsoleClassification {
                    state: ConsoleState::Finished,
                    dot_state: AgentState::Idle,
                    detail: format!("prompt_stable_quiet_{}", quiet as u32),
                    prompt: prompt_line,
                };
            }
            // Recently updated prompt => working (just finished streaming)
            return ConsoleClassification {
                state: ConsoleState::WorkingSingle,
                dot_state: AgentState::Working,
                detail: format!("prompt_recent_quiet_{}", quiet as u32),
                prompt: prompt_line,
            };
        }

        // No timing data, prompt visible => idle
        return ConsoleClassification {
            state: ConsoleState::Finished,
            dot_state: AgentState::Idle,
            detail: "prompt_visible_no_timing".to_string(),
            prompt: prompt_line,
        };
    }

    // 4. No prompt visible
    let has_content = content_is_fresh_output(raw_ansi);
    let has_colored = has_color_activity(raw_ansi);

    if has_content {
        if has_colored {
            return ConsoleClassification {
                state: ConsoleState::WorkingDouble,
                dot_state: AgentState::Working,
                detail: "colored_content_delta".to_string(),
                prompt: prompt_line,
            };
        } else {
            return ConsoleClassification {
                state: ConsoleState::WorkingSingle,
                dot_state: AgentState::Working,
                detail: "plain_content_delta".to_string(),
                prompt: prompt_line,
            };
        }
    }

    // 5. Quiet, no prompt -- breathing
    if let Some(quiet) = quiet_age_secs {
        if quiet > 30.0 {
            return ConsoleClassification {
                state: ConsoleState::Breathing,
                dot_state: AgentState::Working,
                detail: format!("quiet_no_prompt_{}s", quiet as u32),
                prompt: None,
            };
        }
        return ConsoleClassification {
            state: ConsoleState::Breathing,
            dot_state: AgentState::Working,
            detail: format!("breathing_quiet_{}s", quiet as u32),
            prompt: None,
        };
    }

    // 6. No evidence
    ConsoleClassification {
        state: ConsoleState::Unknown,
        dot_state: AgentState::Unknown,
        detail: "no_evidence".to_string(),
        prompt: None,
    }
}

// ---------------------------------------------------------------------------
// Manifest-compatible detection interface for Tau/TauRlm
// ---------------------------------------------------------------------------

/// Classify and return an AgentDetection for integration into the detection pipeline.
pub fn detect_tau(agent: Agent, raw_ansi: &str, quiet_age_secs: Option<f64>) -> AgentDetection {
    let _ = agent; // agent kind doesn't change classification logic
    let cls = classify(raw_ansi, quiet_age_secs);
    AgentDetection {
        state: cls.dot_state,
        skip_state_update: false,
        visible_idle: cls.state == ConsoleState::Finished,
        visible_blocker: cls.state == ConsoleState::Blocked,
        visible_working: matches!(
            cls.state,
            ConsoleState::WorkingSingle | ConsoleState::WorkingDouble | ConsoleState::Breathing
        ),
    }
}

/// Produce metadata tokens for the console classification.
#[allow(dead_code)] // consumed by integration layer at runtime
pub fn metadata_tokens(cls: &ConsoleClassification) -> Vec<(String, String)> {
    vec![
        (
            "tau-console-state".to_string(),
            cls.state.label().to_string(),
        ),
        ("tau-console-detail".to_string(), cls.detail.clone()),
    ]
}

#[cfg(test)]
#[path = "console_state_tests.rs"]
mod console_state_tests;
