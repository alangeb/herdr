use std::collections::{HashMap, HashSet, VecDeque};

mod actions;
mod agent_sidebar;
mod aggregate_navigation;
mod machine_diagnostics;
mod workspace_navigation;
use workspace_navigation::{PendingWorkspaceHighlight, WorkspaceNavigationTarget};
mod composition;
mod config;
mod context_menu;
mod copy_mode;
mod endpoint_agent_state;
mod endpoint_agents;
mod endpoint_navigation;
mod endpoint_notices;
mod endpoint_sidebar;
mod endpoints;
pub(super) use endpoints::*;
mod global_menu;
mod graphics;
mod input;
mod input_source;
mod link_hover;
mod mobile;
mod mouse;
mod notification_policy;
mod notifications;
mod overlay_input;
mod preferences;
mod render;
mod scroll;
mod settings;
mod state;
mod surface_patch;
mod text_editor;
mod word_selection;
mod worktrees;
use text_editor::TextEditor;
use word_selection::ClientWordSelection;

pub(in crate::client::shell) use render::sidebar;
pub(crate) use state::*;
#[cfg(test)]
pub(super) use surface_patch::apply_composed_surface_patch;
pub(super) use surface_patch::{ClientComposedSurfacePatch, ClientPaneSurfacePatchOutcome};

use crossterm::event::KeyCode;
#[cfg(test)]
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use super::endpoint::{ClientEndpointId, ClientEndpointStatus, SavedSshEndpoint};
use crate::app::state::Palette;
use crate::config::{
    Config, LiveKeybindConfig, SidebarCollapsedModeConfig, SpacesSidebarConfig,
    TabBarPositionConfig,
};
use crate::protocol::{
    ClientMessage, ClientMousePosition, ClientPaneInputEvent, ClientShellSnapshot, ClientShellTab,
    ClientShellWorkspace, ClientSurfaceSize, FrameData, PaneSurfaceFrame, SemanticNotification,
    SemanticNotificationKind, SemanticNotificationSound,
};
#[cfg(test)]
use crate::raw_input::RawInputEvent;

fn target_event_message(target: ClientInputTarget, event: ClientPaneInputEvent) -> ClientMessage {
    match target {
        ClientInputTarget::Pane(pane_id) => ClientMessage::ClientShellPaneInput {
            pane_id,
            events: vec![event],
        },
        ClientInputTarget::Popup(terminal_id) => ClientMessage::ClientShellPopupInput {
            terminal_id,
            events: vec![event],
        },
    }
}

fn push_target_event(
    target: ClientInputTarget,
    event: ClientPaneInputEvent,
    outcome: &mut ClientShellInput,
) {
    match target {
        ClientInputTarget::Pane(pane_id) => {
            if let Some(ClientMessage::ClientShellPaneInput {
                pane_id: pending_pane,
                events,
            }) = outcome.requests.last_mut()
            {
                if *pending_pane == pane_id {
                    events.push(event);
                    return;
                }
            }
            outcome.requests.push(target_event_message(
                ClientInputTarget::Pane(pane_id),
                event,
            ));
        }
        ClientInputTarget::Popup(terminal_id) => {
            if let Some(ClientMessage::ClientShellPopupInput {
                terminal_id: pending_terminal,
                events,
            }) = outcome.requests.last_mut()
            {
                if *pending_terminal == terminal_id {
                    events.push(event);
                    return;
                }
            }
            outcome.requests.push(target_event_message(
                ClientInputTarget::Popup(terminal_id),
                event,
            ));
        }
    }
}

fn contains(rect: Rect, point: (u16, u16)) -> bool {
    rect.width > 0
        && rect.height > 0
        && point.0 >= rect.x
        && point.0 < rect.right()
        && point.1 >= rect.y
        && point.1 < rect.bottom()
}

fn pane_surface_topology_signature(surface: &PaneSurfaceFrame) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn write(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(PRIME);
        }
        *hash ^= 0xff;
        *hash = hash.wrapping_mul(PRIME);
    }

    let mut pane_ids = surface
        .panes
        .iter()
        .map(|pane| pane.pane_id.as_bytes())
        .collect::<Vec<_>>();
    pane_ids.sort_unstable();
    let mut hash = OFFSET;
    for pane_id in pane_ids {
        write(&mut hash, pane_id);
    }
    let mut splits = surface.splits.iter().collect::<Vec<_>>();
    splits.sort_by(|left, right| left.path.cmp(&right.path));
    for split in splits {
        write(
            &mut hash,
            &[match split.direction {
                crate::protocol::PaneSurfaceSplitDirection::Horizontal => 0,
                crate::protocol::PaneSurfaceSplitDirection::Vertical => 1,
            }],
        );
        write(
            &mut hash,
            &split
                .path
                .iter()
                .map(|right| u8::from(*right))
                .collect::<Vec<_>>(),
        );
    }
    hash
}

pub(crate) const SPINNER_FRAMES: [&str; 8] =
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
/// Shape garnish: at crawl speed the spin fades to this sparse set, so slow
/// differs from fast in shape as well as tempo.
pub(crate) const SPARSE_FRAMES: [&str; 4] = ["⠁", "⠂", "⠄", "⠈"];
pub(crate) const STALL_FRAMES: [&str; 2] = ["⠐", "⠒"];
pub(crate) const STALL_ORANGE: ratatui::style::Color =
    ratatui::style::Color::Rgb(254, 165, 0);

/// Churn-adaptive spinner state. The client render path is single-threaded
/// (one tokio runtime thread drives tick and paint); a process-global keeps
/// the free render functions (`status_icon`, sidebars) signature-compatible.
/// Per-workspace spinner state. Only the focused workspace receives patch
/// signal (background panes stream no surface data to the client); other
/// working rows animate at a constant rate and can never stall-alarm.
#[derive(Debug)]
pub(crate) struct WorkspaceSpin {
    pub(crate) phase: f64,
    pub(crate) pulse_phase: f64,
    pub(crate) period_ms: f64,
    pub(crate) churn_ema: f64,
    pub(crate) last_growth: Option<std::time::Instant>,
    pub(crate) stalled: bool,
}

impl WorkspaceSpin {
    fn new() -> Self {
        Self {
            phase: 0.0,
            pulse_phase: 0.0,
            period_ms: 400.0,
            churn_ema: 0.0,
            last_growth: None,
            stalled: false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct SpinnerAnim {
    by_workspace: std::collections::HashMap<String, WorkspaceSpin>,
    /// Last observed per-workspace content_seq totals from server snapshots.
    seq_base: std::collections::HashMap<String, u64>,
    last_tick: Option<std::time::Instant>,
    /// Rendered-glyph key across all working rows; repaints only on change.
    last_key: Option<Vec<(String, bool, usize, usize)>>,
}

impl SpinnerAnim {
    fn new() -> Self {
        Self {
            by_workspace: std::collections::HashMap::new(),
            seq_base: std::collections::HashMap::new(),
            last_tick: None,
            last_key: None,
        }
    }
}

static SPINNER_ANIM: std::sync::OnceLock<std::sync::RwLock<SpinnerAnim>> =
    std::sync::OnceLock::new();

pub(crate) fn spinner_anim() -> &'static std::sync::RwLock<SpinnerAnim> {
    SPINNER_ANIM.get_or_init(|| std::sync::RwLock::new(SpinnerAnim::new()))
}

/// Tunable spin model (env overrides for local experimentation only).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SpinTuning {
    pub(crate) floor_ms: f64,
    pub(crate) ceil_ms: f64,
    pub(crate) sparse_from_ms: f64,
    pub(crate) tau_attack_s: f64,
    pub(crate) tau_decay_s: f64,
}

impl SpinTuning {
    const fn defaults() -> Self {
        Self {
            floor_ms: 150.0,
            ceil_ms: 2400.0,
            sparse_from_ms: 1600.0,
            tau_attack_s: 0.4,
            tau_decay_s: 1.5,
        }
    }

    fn from_env() -> Self {
        fn get(key: &str, fallback: f64) -> f64 {
            std::env::var(key)
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(fallback)
        }
        let d = Self::defaults();
        let t = Self {
            floor_ms: get("HERDR_SPIN_FLOOR_MS", d.floor_ms),
            ceil_ms: get("HERDR_SPIN_CEIL_MS", d.ceil_ms),
            sparse_from_ms: get("HERDR_SPIN_SPARSE_MS", d.sparse_from_ms),
            tau_attack_s: get("HERDR_SPIN_TAU_ATTACK_S", d.tau_attack_s),
            tau_decay_s: get("HERDR_SPIN_TAU_DECAY_S", d.tau_decay_s),
        };
        Self {
            floor_ms: t.floor_ms.min(t.ceil_ms),
            sparse_from_ms: t.sparse_from_ms.clamp(t.floor_ms, t.ceil_ms),
            ..t
        }
    }
}

pub(crate) static SPIN_TUNING: std::sync::OnceLock<SpinTuning> = std::sync::OnceLock::new();

pub(crate) fn spin_tuning() -> SpinTuning {
    SPIN_TUNING.get_or_init(SpinTuning::from_env).to_owned()
}

/// Content-churn (EMA of patches/sec) to spin period in ms. Steeper than
/// 1/x so slow vs. fast reading is obvious; saturates at the fast floor.
pub(crate) fn churn_period_ms(churn_ema: f64) -> f64 {
    let t = spin_tuning();
    let ema = churn_ema.max(0.0);
    (3200.0 / (1.0 + ema) * (1.0 / (1.0 + ema))).clamp(t.floor_ms, t.ceil_ms)
}

/// Pure glyph selection so tests do not touch the shared anim state.
pub(crate) fn working_glyph(period_ms: f64, phase: f64, stalled: bool) -> &'static str {
    let frame = phase.max(0.0).floor() as usize;
    if stalled {
        STALL_FRAMES[frame % STALL_FRAMES.len()]
    } else if period_ms >= spin_tuning().sparse_from_ms {
        SPARSE_FRAMES[frame % SPARSE_FRAMES.len()]
    } else {
        SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
    }
}

/// Called for every arriving pane-surface frame/patch, attributed to the
/// workspace that currently owns the visible pane. Bursts push that entry's
/// churn EMA quickly (short attack time-constant); decay is continuous.
pub(crate) fn spinner_stalled_for(workspace_id: &str) -> bool {
    spinner_anim()
        .read()
        .ok()
        .and_then(|a| a.by_workspace.get(workspace_id).map(|e| e.stalled))
        .unwrap_or(false)
}

/// Per-workspace animated glyph for Working+Spinners; None means the caller
/// should fall back to the static `status_icon` mapping (other styles).
pub(crate) fn working_glyph_for(
    workspace_id: &str,
    status: crate::api::schema::AgentStatus,
    style: crate::config::StatusIndicatorStyle,
) -> Option<&'static str> {
    if status != crate::api::schema::AgentStatus::Working
        || style != crate::config::StatusIndicatorStyle::Spinners
    {
        return None;
    }
    Some(match spinner_anim().read() {
        Ok(a) => match a.by_workspace.get(workspace_id) {
            Some(e) if e.stalled => working_glyph(e.period_ms, e.pulse_phase, true),
            Some(e) => working_glyph(e.period_ms, e.phase, false),
            None => SPINNER_FRAMES[0],
        },
        Err(_) => SPINNER_FRAMES[0],
    })
}

fn status_icon(
    status: crate::api::schema::AgentStatus,
    style: crate::config::StatusIndicatorStyle,
) -> &'static str {
    use crate::api::schema::AgentStatus;
    use crate::config::StatusIndicatorStyle;
    match (style, status) {
        (
            StatusIndicatorStyle::Dots,
            AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done,
        ) => "●",
        (StatusIndicatorStyle::Dots, AgentStatus::Idle) => "○",
        (StatusIndicatorStyle::Dots, AgentStatus::Unknown) => "·",
        (StatusIndicatorStyle::Symbols, AgentStatus::Blocked) => "×",
        (StatusIndicatorStyle::Symbols, AgentStatus::Working) => "◐",
        (StatusIndicatorStyle::Symbols, AgentStatus::Done) => "✓",
        (StatusIndicatorStyle::Symbols, AgentStatus::Idle) => "○",
        (StatusIndicatorStyle::Symbols, AgentStatus::Unknown) => "·",
        (StatusIndicatorStyle::Spinners, AgentStatus::Blocked) => "×",
        (StatusIndicatorStyle::Spinners, AgentStatus::Working) => {
            // No workspace context at this call site (mobile/overlays/agent
            // panel): static frame; the workspace sidebar uses per-workspace
            // `working_glyph_for`.
            SPINNER_FRAMES[0]
        }
        (StatusIndicatorStyle::Spinners, AgentStatus::Done) => "✓",
        (StatusIndicatorStyle::Spinners, AgentStatus::Idle) => "○",
        (StatusIndicatorStyle::Spinners, AgentStatus::Unknown) => "·",
    }
}

fn status_dot(status: crate::api::schema::AgentStatus) -> &'static str {
    status_icon(status, crate::config::StatusIndicatorStyle::Dots)
}

fn status_priority(status: crate::api::schema::AgentStatus) -> u8 {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

fn status_text(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    }
}

fn status_color(
    status: crate::api::schema::AgentStatus,
    palette: &Palette,
) -> ratatui::style::Color {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Working => palette.yellow,
        AgentStatus::Blocked => palette.red,
        AgentStatus::Done => palette.teal,
        AgentStatus::Idle => palette.green,
        AgentStatus::Unknown => palette.overlay0,
    }
}

fn panel_contrast_fg(palette: &Palette) -> ratatui::style::Color {
    match palette.panel_bg {
        ratatui::style::Color::Reset => palette.surface_dim,
        color => color,
    }
}

fn blit_pane_surface(target: &mut FrameData, source: &FrameData, area: Rect) {
    let copy_width = source.width.min(area.width);
    let copy_height = source.height.min(area.height);
    let hyperlink_base = target.hyperlinks.len() as u32;
    target.hyperlinks.extend(source.hyperlinks.iter().cloned());

    for row in 0..copy_height {
        for col in 0..copy_width {
            let source_index = row as usize * source.width as usize + col as usize;
            let target_x = area.x + col;
            let target_y = area.y + row;
            let target_index = target_y as usize * target.width as usize + target_x as usize;
            let (Some(source_cell), Some(target_cell)) = (
                source.cells.get(source_index),
                target.cells.get_mut(target_index),
            ) else {
                continue;
            };
            *target_cell = source_cell.clone();
            target_cell.hyperlink = source_cell.hyperlink.and_then(|index| {
                ((index as usize) < source.hyperlinks.len()).then_some(hyperlink_base + index)
            });
        }
    }

    target.cursor = source.cursor.as_ref().and_then(|cursor| {
        (cursor.x < copy_width && cursor.y < copy_height).then(|| crate::protocol::CursorState {
            x: area.x + cursor.x,
            y: area.y + cursor.y,
            visible: cursor.visible,
            shape: cursor.shape,
        })
    });
    target.graphics.clear();
}

#[cfg(test)]
mod tests;
