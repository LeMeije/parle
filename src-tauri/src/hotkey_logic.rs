//! Pure hold/toggle/hybrid gesture state machine. Platform listeners feed raw
//! down/up events; this decides Start/Stop/Cancel. No timers of its own — the
//! caller supplies timestamps, which keeps it deterministic and testable.

use echokey_core::settings::HotkeyMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureAction {
    StartRecording,
    StopRecording,
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GestureState {
    Idle,
    /// Key held, recording started, not yet latched.
    HoldRecording { down_at_ms: u64 },
    /// Latched into toggle (short tap in Hybrid, or Toggle mode).
    ToggleRecording,
}

pub struct GestureMachine {
    mode: HotkeyMode,
    latch_ms: u64,
    state: GestureState,
}

impl GestureMachine {
    pub fn new(mode: HotkeyMode, latch_ms: u64) -> Self {
        Self { mode, latch_ms, state: GestureState::Idle }
    }

    pub fn set_mode(&mut self, mode: HotkeyMode, latch_ms: u64) {
        self.mode = mode;
        self.latch_ms = latch_ms;
        self.state = GestureState::Idle;
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.state, GestureState::Idle)
    }

    /// True only while the key is physically held and not yet latched.
    pub fn in_hold_phase(&self) -> bool {
        matches!(self.state, GestureState::HoldRecording { .. })
    }

    /// External stop (Escape, HUD click, error): reset to idle.
    pub fn reset(&mut self) {
        self.state = GestureState::Idle;
    }

    pub fn on_key(&mut self, phase: KeyPhase, now_ms: u64) -> GestureAction {
        use GestureAction::*;
        use GestureState::*;
        match (self.mode, self.state, phase) {
            // -- Hold: down starts, up stops.
            (HotkeyMode::Hold, Idle, KeyPhase::Down) => {
                self.state = HoldRecording { down_at_ms: now_ms };
                StartRecording
            }
            (HotkeyMode::Hold, HoldRecording { .. }, KeyPhase::Up) => {
                self.state = Idle;
                StopRecording
            }

            // -- Toggle: each full tap flips.
            (HotkeyMode::Toggle, Idle, KeyPhase::Down) => {
                self.state = ToggleRecording;
                StartRecording
            }
            (HotkeyMode::Toggle, ToggleRecording, KeyPhase::Down) => {
                self.state = Idle;
                StopRecording
            }
            (HotkeyMode::Toggle, _, KeyPhase::Up) => Nothing,

            // -- Hybrid: down starts; quick release latches, long hold = PTT.
            (HotkeyMode::Hybrid, Idle, KeyPhase::Down) => {
                self.state = HoldRecording { down_at_ms: now_ms };
                StartRecording
            }
            (HotkeyMode::Hybrid, HoldRecording { down_at_ms }, KeyPhase::Up) => {
                if now_ms.saturating_sub(down_at_ms) < self.latch_ms {
                    self.state = ToggleRecording; // latched; keep recording
                    Nothing
                } else {
                    self.state = Idle;
                    StopRecording
                }
            }
            (HotkeyMode::Hybrid, ToggleRecording, KeyPhase::Down) => {
                self.state = Idle;
                StopRecording
            }

            // Repeats and stray events.
            (_, HoldRecording { .. }, KeyPhase::Down) => Nothing, // key auto-repeat
            (_, Idle, KeyPhase::Up) => Nothing,
            (HotkeyMode::Hybrid, ToggleRecording, KeyPhase::Up) => Nothing,
            // Unreachable in practice (mode changes reset state), but total.
            (_, ToggleRecording, _) => Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GestureAction::*;
    use KeyPhase::*;

    #[test]
    fn hold_mode() {
        let mut m = GestureMachine::new(HotkeyMode::Hold, 450);
        assert_eq!(m.on_key(Down, 0), StartRecording);
        assert_eq!(m.on_key(Down, 100), Nothing); // auto-repeat
        assert_eq!(m.on_key(Up, 2000), StopRecording);
        assert!(!m.is_active());
    }

    #[test]
    fn toggle_mode() {
        let mut m = GestureMachine::new(HotkeyMode::Toggle, 450);
        assert_eq!(m.on_key(Down, 0), StartRecording);
        assert_eq!(m.on_key(Up, 80), Nothing);
        assert_eq!(m.on_key(Down, 3000), StopRecording);
        assert_eq!(m.on_key(Up, 3080), Nothing);
    }

    #[test]
    fn hybrid_long_hold_is_ptt() {
        let mut m = GestureMachine::new(HotkeyMode::Hybrid, 450);
        assert_eq!(m.on_key(Down, 0), StartRecording);
        assert_eq!(m.on_key(Up, 1200), StopRecording);
    }

    #[test]
    fn hybrid_quick_tap_latches() {
        let mut m = GestureMachine::new(HotkeyMode::Hybrid, 450);
        assert_eq!(m.on_key(Down, 0), StartRecording);
        assert_eq!(m.on_key(Up, 200), Nothing); // latched
        assert!(m.is_active());
        assert_eq!(m.on_key(Down, 5000), StopRecording);
        assert_eq!(m.on_key(Up, 5100), Nothing);
    }

    #[test]
    fn reset_from_external_stop() {
        let mut m = GestureMachine::new(HotkeyMode::Hybrid, 450);
        m.on_key(Down, 0);
        m.on_key(Up, 100); // latched
        m.reset(); // e.g. Escape pressed
        assert!(!m.is_active());
        assert_eq!(m.on_key(Down, 9000), StartRecording);
    }
}
