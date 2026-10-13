//! Frees the speech model while Parle is not dictating; `Work::Preload` loads
//! it again the moment a recording starts.
//!
//! Why: the model IS the app's memory. Measured 13/10/2026 on an M2 (24 GB)
//! with Whisper Large v3 Turbo q5_0: the weights were 548 MB of the app's
//! 607 MB footprint, held for as long as the app ran, dictating or not. Nor
//! were they warm in any useful sense: ggml-metal asks macOS to keep them
//! resident for 3 minutes after each use, then stops asking, and when the
//! idle app was measured, 547.1 of those 547.8 MB were swapped or compressed
//! out of RAM. The next dictation had to bring them back either way.
//!
//! What freeing them costs: 35 timed reloads had a median of 0.59 s (0.21 to
//! 1.6 s, the three slowest in one run with other builds saturating the
//! machine), with the file in the OS cache; evicted from the cache, so read
//! from the SSD, it took 0.87 s against 0.43 to 0.77 s cached in the same run.
//! ggml-metal's device, shader library and
//! compiled pipelines live in a process-wide singleton and survive a release,
//! so the first pass after a reload was within the noise of a warm one. The
//! memory really comes back: in the standalone probe (`parle-asr` example
//! `memory`) the footprint was 640 MB loaded and 38 to 44 MB once macOS had
//! reclaimed the freed pages, which it does lazily: within 10 s in one run,
//! 30 s in another with the machine heavily loaded.
//!
//! The reload starts with the recording, so it runs while the user is still
//! talking. Across 1,549 real dictations, no recording that followed a gap of
//! over five minutes was shorter than one second, and 95% ran past 6.4 s.
//!
//! Freed sooner under memory pressure: 30 s after the last use when the kernel
//! reports a warning, as soon as nothing is in flight when it reports critical.
//! Never while a recording is live or a take is still queued.

use crate::pipeline::Pipeline;
use crate::platform::{self, MemoryPressure};
use parking_lot::Mutex;
use parle_asr::manager::EngineManager;
use parle_core::settings::Settings;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// How often the reaper looks. This only decides how LATE a release can be,
/// never how early: one sysctl and one try_lock every ten seconds.
const TICK: Duration = Duration::from_secs(10);

/// The settled footprint is logged this many ticks after a release, because
/// macOS reclaims the freed pages lazily (10 to 30 s measured).
const SETTLE_TICKS: u32 = 3;

/// Under a pressure warning the model goes this long after its last use
/// instead of after the full timeout. Holding it longer buys little there: on
/// 13/10/2026, at warning level, 547.1 of its 547.8 MB were already swapped or
/// compressed out 48 s after a dictation was saved, despite ggml's residency
/// requests, so the next take would have read it back from swap anyway.
/// Released before macOS evicts it, it is never written out at all; 48 s is
/// only an upper bound on when that eviction happened, so this is a best
/// effort. Thirty seconds still lets a quick follow-up reuse it.
const PRESSURE_GRACE: Duration = Duration::from_secs(30);

/// Whether to free the model now. Pure, so every rule is pinned by a test.
pub fn should_release(idle: Duration, idle_minutes: u32, pressure: MemoryPressure, busy: bool) -> bool {
    // "Never" means never, pressure or not: it is the old behaviour, on request.
    if idle_minutes == 0 || busy {
        return false;
    }
    let timeout = Duration::from_secs(u64::from(idle_minutes) * 60);
    let limit = match pressure {
        MemoryPressure::Normal => timeout,
        MemoryPressure::Warn => PRESSURE_GRACE,
        MemoryPressure::Critical => Duration::ZERO,
    };
    idle >= limit
}

pub fn spawn(
    app: AppHandle,
    settings: Arc<Mutex<Settings>>,
    engine: Arc<Mutex<EngineManager>>,
    pipeline: Arc<Pipeline>,
) {
    let spawned = std::thread::Builder::new().name("parle-model-idle".into()).spawn(move || {
        // (footprint just before a release, ticks left before reporting it)
        let mut report: Option<(u64, u32)> = None;
        loop {
            std::thread::sleep(TICK);
            // Where the footprint SETTLED, SETTLE_TICKS after a release. Read
            // straight after the drop it overstates what is held, and read
            // after a recording has reloaded the model it would be wrong the
            // other way, so it is only reported while the model is still out.
            if let Some((before, ticks)) = report.take() {
                if ticks > 1 {
                    report = Some((before, ticks - 1));
                } else if still_released(&engine, &pipeline) {
                    if let Some(after) = platform::footprint_bytes() {
                        tracing::info!(
                            "app footprint {} MB just before the release, {} MB {} s later",
                            mb(before),
                            mb(after),
                            TICK.as_secs() * u64::from(SETTLE_TICKS)
                        );
                    }
                }
            }
            if let Some(before) = tick(&app, &settings, &engine, &pipeline) {
                report = Some((before, SETTLE_TICKS));
            }
        }
    });
    if let Err(e) = spawned {
        // Not fatal: the app simply keeps the model loaded, as it always did.
        tracing::error!("could not start the idle model reaper: {e}");
    }
}

/// One look. Returns the footprint from just before a release, if one happened.
fn tick(
    app: &AppHandle,
    settings: &Mutex<Settings>,
    engine: &Mutex<EngineManager>,
    pipeline: &Pipeline,
) -> Option<u64> {
    let minutes = settings.lock().models.idle_unload_minutes;
    if minutes == 0 || pipeline.is_busy() {
        return None;
    }
    // try_lock: a held engine is an engine in use (a pass, a load, the startup
    // warm-up), which is the opposite of idle. Never wait for it.
    let mut guard = engine.try_lock()?;
    let idle = guard.idle_for()?;
    let pressure = platform::memory_pressure();
    // `is_busy` again, under the engine lock: a recording that started since
    // the first look has queued a preload, which is now waiting on this lock.
    if !should_release(idle, minutes, pressure, pipeline.is_busy()) {
        return None;
    }
    let before = platform::footprint_bytes();
    let id = guard.release()?;
    let status = guard.status();
    drop(guard);
    tracing::info!(
        "freed model {id} after {} s without use (memory pressure {pressure:?}); it reloads when a recording starts",
        idle.as_secs()
    );
    let _ = app.emit("engine-status", status);
    before
}

/// Nothing recording or queued, and no model in memory.
fn still_released(engine: &Mutex<EngineManager>, pipeline: &Pipeline) -> bool {
    !pipeline.is_busy() && engine.try_lock().is_some_and(|g| g.idle_for().is_none())
}

fn mb(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn never_means_never_even_under_critical_pressure() {
        for pressure in [MemoryPressure::Normal, MemoryPressure::Warn, MemoryPressure::Critical] {
            assert!(!should_release(Duration::from_secs(86_400), 0, pressure, false), "{pressure:?}");
        }
    }

    #[test]
    fn a_live_recording_or_queued_take_is_never_interrupted() {
        for pressure in [MemoryPressure::Normal, MemoryPressure::Warn, MemoryPressure::Critical] {
            assert!(!should_release(Duration::from_secs(86_400), 5, pressure, true), "{pressure:?}");
        }
    }

    #[test]
    fn normal_pressure_waits_for_the_full_timeout_and_not_longer() {
        // Pinned from both sides: one second short keeps it, the timeout frees it.
        assert!(!should_release(5 * MIN - Duration::from_secs(1), 5, MemoryPressure::Normal, false));
        assert!(should_release(5 * MIN, 5, MemoryPressure::Normal, false));
        assert!(should_release(60 * MIN, 5, MemoryPressure::Normal, false));
        assert!(!should_release(14 * MIN, 15, MemoryPressure::Normal, false));
        assert!(should_release(15 * MIN, 15, MemoryPressure::Normal, false));
    }

    #[test]
    fn a_pressure_warning_shortens_the_wait_to_the_grace_period() {
        assert!(!should_release(PRESSURE_GRACE - Duration::from_secs(1), 60, MemoryPressure::Warn, false));
        assert!(should_release(PRESSURE_GRACE, 60, MemoryPressure::Warn, false));
    }

    #[test]
    fn a_pressure_warning_can_only_shorten_the_wait() {
        // The setting is in whole minutes, so a grace under one minute is
        // shorter than every timeout a user can pick. Raise it past that and
        // the Warn arm needs `.min(timeout)` back.
        assert!(PRESSURE_GRACE < MIN);
    }

    #[test]
    fn critical_pressure_frees_it_as_soon_as_nothing_is_in_flight() {
        assert!(should_release(Duration::ZERO, 60, MemoryPressure::Critical, false));
    }
}
