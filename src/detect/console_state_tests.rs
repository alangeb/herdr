use super::*;

fn esc(s: &str) -> String {
    format!("\x1b{}", s)
}

fn esc_pair(open: &str, text: &str, close: &str) -> String {
    format!("{}{}{}", esc(open), text, esc(close))
}

// ── strip_ansi ──

#[test]
fn strip_ansi_removes_csi() {
    let input = format!("{}>>>{} hello", esc("[38;5;14m"), esc("[0m"));
    assert_eq!(strip_ansi(&input), ">>> hello");
}

#[test]
fn strip_ansi_preserves_plain() {
    assert_eq!(strip_ansi("hello world"), "hello world");
}

// ── prompt detection ──

#[test]
fn detect_tau_prompt_bare() {
    let lines = vec!["some output", "", ">>>"];
    let result = detect_prompt(&lines);
    assert!(result.is_some());
    assert!(result.unwrap().0.contains(">>>"));
}

#[test]
fn detect_rlm_prompt_with_number() {
    let lines = vec!["output", "rlm[42]>>>"];
    let result = detect_prompt(&lines);
    assert!(result.is_some());
    assert!(result.unwrap().0.contains("rlm[42]>>>"));
}

#[test]
fn detect_prompt_with_suffix() {
    let lines = vec![">>> print(1+1)"];
    let result = detect_prompt(&lines);
    assert!(result.is_some());
    assert!(result.unwrap().0.contains("print(1+1)"));
}

#[test]
fn prompt_bare_no_suffix() {
    assert!(!prompt_has_suffix(">>>"));
    assert!(!prompt_has_suffix("  >>>"));
    assert!(!prompt_has_suffix(">>>   "));
}

#[test]
fn prompt_with_typed_input_has_suffix() {
    assert!(prompt_has_suffix(">>> run cargo test"));
}

// ── blocked detection ──

#[test]
fn blocked_waiting_for_input() {
    assert!(detect_blocked(&["Waiting for input..."]).is_some());
}

#[test]
fn blocked_confirm() {
    assert!(detect_blocked(&["Apply patch? confirm[y/n]"]).is_some());
}

#[test]
fn blocked_press_enter() {
    assert!(detect_blocked(&["Press enter to continue"]).is_some());
}

#[test]
fn blocked_awaiting() {
    assert!(detect_blocked(&["awaiting response..."]).is_some());
}

#[test]
fn blocked_permission() {
    assert!(detect_blocked(&["Permission required: file write"]).is_some());
}

#[test]
fn blocked_not_in_diff() {
    let lines = vec![
        "+// waiting for next packet",
        "- waiting code",
        "  more code",
    ];
    assert!(detect_blocked(&lines).is_none());
}

// ── error detection: visible red text only ──

#[test]
fn error_plain_word_error_is_not_error() {
    assert!(detect_error("ERROR: ordinary prose containing the word error").is_none());
    assert!(detect_error("state: error in unrelated tool output").is_none());
}

#[test]
fn error_bright_red_text_is_error() {
    assert!(detect_error(&esc_pair("[91m", "boom", "[0m")).is_some());
}

#[test]
fn error_normal_red_text_is_error() {
    assert!(detect_error(&esc_pair("[31m", "boom", "[0m")).is_some());
}

#[test]
fn error_256_red_text_is_error() {
    assert!(detect_error(&esc_pair("[38;5;9m", "boom", "[0m")).is_some());
}

#[test]
fn error_truecolor_red_text_is_error() {
    assert!(detect_error(&esc_pair("[38;2;218;78;78m", "boom", "[0m")).is_some());
}

#[test]
fn error_red_spaces_only_is_not_error() {
    assert!(detect_error(&esc_pair("[91m", "    ", "[0m")).is_none());
}

#[test]
fn error_red_background_is_not_error() {
    assert!(detect_error(&esc_pair("[41m", "boom", "[0m")).is_none());
}

#[test]
fn error_orange_truecolor_is_not_error() {
    assert!(detect_error(&esc_pair("[38;2;255;165;0m", "orange", "[0m")).is_none());
}

// ── state mapping ──

#[test]
fn state_mapping_to_dot() {
    assert_eq!(ConsoleState::Finished.to_agent_state(), AgentState::Idle);
    assert_eq!(
        ConsoleState::WorkingSingle.to_agent_state(),
        AgentState::Working
    );
    assert_eq!(
        ConsoleState::WorkingDouble.to_agent_state(),
        AgentState::Working
    );
    assert_eq!(
        ConsoleState::Breathing.to_agent_state(),
        AgentState::Working
    );
    assert_eq!(ConsoleState::Blocked.to_agent_state(), AgentState::Blocked);
    assert_eq!(ConsoleState::Error.to_agent_state(), AgentState::Unknown);
    assert_eq!(ConsoleState::Unknown.to_agent_state(), AgentState::Unknown);
}

// ── classify: idle prompt ──

#[test]
fn classify_prompt_idle() {
    let raw = format!("{}>>>{}", esc("[38;5;14m"), esc("[0m"));
    let cls = classify(&raw, Some(5.0));
    assert_eq!(cls.state, ConsoleState::Finished);
    assert_eq!(cls.dot_state, AgentState::Idle);
}

#[test]
fn classify_rlm_prompt_idle() {
    let raw = esc_pair("[38;5;14m", "rlm[42]>>>", "[0m");
    let cls = classify(&raw, Some(3.0));
    assert_eq!(cls.state, ConsoleState::Finished);
}

// ── classify: prompt+typing idle ──

#[test]
fn classify_prompt_typing_idle() {
    let raw = format!("{}>>>{} run cargo test", esc("[38;5;14m"), esc("[0m"));
    let cls = classify(&raw, Some(2.0));
    assert_eq!(cls.state, ConsoleState::Finished);
    assert_eq!(cls.dot_state, AgentState::Idle);
}

#[test]
fn classify_prompt_suffix_not_working() {
    let raw = esc_pair("[38;5;14m", ">>> ", "[0m") + "foo.py --verbose --flag";
    let cls = classify(&raw, Some(2.0));
    assert_eq!(cls.state, ConsoleState::Finished);
}

// ── classify: status-bar/tool update working ──

#[test]
fn classify_statusbar_update_working() {
    let raw = "[working] running tests 3/50";
    let cls = classify(raw, Some(0.5));
    assert_eq!(cls.dot_state, AgentState::Working);
}

// ── classify: content change → working ──

#[test]
fn classify_colored_content_working_double() {
    let raw = format!(
        "{}\n{}",
        esc_pair("[32m", "test result: ok. 42 passed", "[0m"),
        esc_pair("[93m", "1 warning", "[0m")
    );
    let cls = classify(&raw, Some(0.5));
    assert_eq!(cls.state, ConsoleState::WorkingDouble);
}

#[test]
fn classify_plain_content_working_single() {
    let raw = "Running test suite...\n42 tests passed, 0 failed";
    let cls = classify(raw, Some(0.5));
    assert_eq!(cls.state, ConsoleState::WorkingSingle);
}

// ── classify: blocked ──

#[test]
fn classify_blocked() {
    let raw = "Some output\nWaiting for input...";
    let cls = classify(raw, Some(5.0));
    assert_eq!(cls.state, ConsoleState::Blocked);
    assert_eq!(cls.dot_state, AgentState::Blocked);
}

// ── classify: error ──

#[test]
fn classify_error() {
    let raw = String::from("Processing...\n") + &esc_pair("[91m", "ERROR: something broke", "[0m");
    let cls = classify(&raw, Some(5.0));
    assert_eq!(cls.state, ConsoleState::Error);
    assert_eq!(cls.dot_state, AgentState::Unknown);
    assert_eq!(cls.detail, "red_text");
}

// ── classify: quiet/breathing ──

#[test]
fn classify_quiet_no_prompt_breathing() {
    let raw = "partial output streaming...\n";
    let cls = classify(raw, Some(10.0));
    assert!(matches!(
        cls.state,
        ConsoleState::Breathing | ConsoleState::WorkingSingle
    ));
    assert_eq!(cls.dot_state, AgentState::Working);
}

// ── classify: empty → unknown ──

#[test]
fn classify_empty_unknown() {
    let cls = classify("", None);
    assert_eq!(cls.state, ConsoleState::Unknown);
}

// ── detect_tau integration ──

#[test]
fn detect_tau_idle() {
    let raw = format!("{}>>>{}", esc("[38;5;14m"), esc("[0m"));
    let det = detect_tau(Agent::Tau, &raw, Some(5.0));
    assert_eq!(det.state, AgentState::Idle);
    assert!(!det.skip_state_update);
    assert!(det.visible_idle);
}

#[test]
fn detect_taurm_idle() {
    let raw = esc_pair("[38;5;14m", "rlm[7]>>>", "[0m");
    let det = detect_tau(Agent::TauRlm, &raw, Some(5.0));
    assert_eq!(det.state, AgentState::Idle);
}

// ── metadata tokens ──

#[test]
fn metadata_tokens_present() {
    let raw = format!("{}>>>{}", esc("[38;5;14m"), esc("[0m"));
    let cls = classify(&raw, Some(5.0));
    let tokens = metadata_tokens(&cls);
    assert!(tokens.iter().any(|(k, _)| k == "tau-console-state"));
    assert!(tokens.iter().any(|(k, _)| k == "tau-console-detail"));
}

#[test]
fn metadata_tokens_error() {
    let cls = classify(
        &esc_pair("[91m", "ERROR: something broke", "[0m"),
        Some(2.0),
    );
    let tokens = metadata_tokens(&cls);
    let st = tokens.iter().find(|(k, _)| k == "tau-console-state");
    assert_eq!(st.unwrap().1, "ERROR");
}

#[test]
fn classify_error_red_beats_prompt() {
    let raw = format!(
        "{}>>>{}\n{}",
        esc("[38;5;14m"),
        esc("[0m"),
        esc_pair("[91m", "ERROR: red visible text", "[0m")
    );
    let cls = classify(&raw, Some(5.0));
    assert_eq!(cls.state, ConsoleState::Error);
    assert_eq!(cls.dot_state, AgentState::Unknown);
}

#[test]
fn classify_plain_error_word_at_prompt_is_idle() {
    let raw = format!(
        "state: error was mentioned earlier\n{}>>>{}",
        esc("[38;5;14m"),
        esc("[0m")
    );
    let cls = classify(&raw, Some(5.0));
    assert_eq!(cls.state, ConsoleState::Finished);
    assert_eq!(cls.dot_state, AgentState::Idle);
}

// ── working dot visible_working ──

#[test]
fn detect_tau_working_has_visible_working() {
    let raw = esc_pair("[32m", "Building...\nCompiling crate v0.1", "[0m");
    let det = detect_tau(Agent::Tau, &raw, Some(0.5));
    assert_eq!(det.state, AgentState::Working);
    assert!(det.visible_working);
}

// ── explain integration: Tau uses console-state, not manifest ──

#[test]
fn explain_tau_working_not_no_live_state_evidence() {
    // A working Tau pane: colored output, no prompt, quiet 0.5s ago
    let raw = "\x1b[32mRunning build...\x1b[0m\nCompiling herdr v0.9.1";
    // The manifest-based explain should NOT be used for Tau.
    // The console_state classifier should be the authority.
    let cls = classify(raw, Some(0.5));
    // Must be classified as working, not idle/unknown
    assert_eq!(cls.dot_state, AgentState::Working);
    assert_ne!(cls.detail, "no_live_state_evidence");
    assert_ne!(cls.detail, "no_evidence");
    // Must not fall through to manifest no_live_state_evidence
    assert!(
        cls.detail.contains("colored_content") || cls.detail.contains("plain_content"),
        "detail should indicate console-state detection, got: {}",
        cls.detail
    );
}

// -- prompt suffix [RUNNING] = working (tau/taurlm agent_input.py turn_active) --

#[test]
fn classify_tau_prompt_running_working() {
    let raw = "\x1b[38;5;14m>>>\x1b[0m [RUNNING] ";
    let cls = classify(raw, Some(30.0));
    assert_eq!(cls.state, ConsoleState::WorkingSingle);
    assert_eq!(cls.dot_state, AgentState::Working);
    assert_eq!(cls.detail, "prompt_status_active");
    assert!(
        cls.prompt.is_some(),
        "prompt token must reflect prompt presence"
    );
    assert!(cls.prompt.as_ref().unwrap().contains("[RUNNING]"));
}

#[test]
fn classify_rlm_prompt_running_working() {
    let raw = "\x1b[38;5;14mrlm[1]>>>\x1b[0m [RUNNING] ";
    let cls = classify(raw, Some(30.0));
    assert_eq!(cls.state, ConsoleState::WorkingSingle);
    assert_eq!(cls.dot_state, AgentState::Working);
    assert_eq!(cls.detail, "prompt_status_active");
    assert!(
        cls.prompt.is_some(),
        "prompt token must reflect prompt presence"
    );
}

#[test]
fn classify_prompt_running_beats_quiet_age() {
    let raw = "prior output\n>>> [RUNNING] ";
    let cls = classify(raw, Some(120.0));
    assert_eq!(cls.dot_state, AgentState::Working);
}

#[test]
fn classify_prompt_running_colored_is_double() {
    let raw = "\x1b[38;5;14m>>>\x1b[0m [RUNNING] \x1b[33mtool output streaming\x1b[0m";
    let cls = classify(raw, Some(1.0));
    assert_eq!(cls.state, ConsoleState::WorkingDouble);
    assert_eq!(cls.dot_state, AgentState::Working);
}

#[test]
fn classify_typed_suffix_still_idle() {
    let raw = ">>> run cargo test";
    let cls = classify(raw, Some(30.0));
    assert_eq!(cls.state, ConsoleState::Finished);
    assert_eq!(cls.dot_state, AgentState::Idle);
}

#[test]
fn running_indicator_not_false_blocked() {
    let raw = ">>> [RUNNING] ";
    let cls = classify(raw, Some(1.0));
    assert_ne!(cls.state, ConsoleState::Blocked);
    assert_eq!(cls.dot_state, AgentState::Working);
}
