//! Burner control from the valves.
//!
//! Each MAX! radiator thermostat already regulates its room: it opens as far as it needs to
//! reach the setpoint (on the wall thermostat's temperature when linked to one). So the
//! burner follows the valves, as Homematic and tado° do: the most-open valve is the heat
//! demand, and a time-proportional (TPI) cycle runs the burner for that share of the time.
//! The C++ firmware ran its own PID on the room temperature on top of the valves' own
//! control, two controllers on one error with separate tuning.

use crate::Millis;
use crate::config::HeatingConfig;

const MINUTE: Millis = 60_000;

/// What the rooms ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Demand {
    /// The most-open valve among rooms, 0-100 %, or 0 below the threshold.
    pub output: f32,
    /// A thermostat is in an active boost.
    pub boost: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Burner {
    pub on: bool,
    last_change: Option<Millis>,
}

impl Burner {
    /// Decide the relay state. The cycle position comes from the monotonic clock, so a
    /// demand of 40 % with a 20 minute cycle runs the burner for the first 8 minutes of
    /// every cycle, stretched to the minimum on and off times.
    pub fn update(&mut self, cfg: &HeatingConfig, demand: Demand, now: Millis) -> bool {
        let cycle = ((cfg.cycle_minutes * MINUTE as f32) as Millis).max(1);
        let position = (now % cycle) as f32 * 100.0 / cycle as f32;
        let want = demand.boost || position < demand.output;

        if want != self.on {
            if let Some(changed) = self.last_change {
                let minimum = if self.on { cfg.min_on_minutes } else { cfg.min_off_minutes };
                let held = now.saturating_sub(changed) < (minimum * MINUTE as f32) as Millis;
                // A boost may cut the minimum off time short, as it did before.
                if held && !(want && demand.boost) {
                    return self.on;
                }
            }
            self.on = want;
            self.last_change = Some(now);
        }
        self.on
    }
}

/// The demand one valve opening makes: nothing below `min_valve_position`, so a valve that
/// is only just open (and whistling) does not keep the burner going.
pub fn valve_demand(cfg: &HeatingConfig, valve: u8) -> f32 {
    if valve < cfg.min_valve_position { 0.0 } else { valve.min(100) as f32 }
}

/// Whether a boost started at `since` is still running at `now`. The burner stops two
/// minutes early, since the valves close at the end of the boost.
pub fn boost_active(boost_minutes: u8, since: Millis, now: Millis) -> bool {
    let duration = (boost_minutes as Millis * MINUTE).saturating_sub(2 * MINUTE);
    now.saturating_sub(since) < duration
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HeatingConfig {
        HeatingConfig::default()
    }

    #[test]
    fn small_openings_do_not_count() {
        let c = cfg();
        assert_eq!(valve_demand(&c, 29), 0.0);
        assert_eq!(valve_demand(&c, 30), 30.0);
        assert_eq!(valve_demand(&c, 100), 100.0);
    }

    #[test]
    fn burner_follows_tpi_cycle() {
        let c = cfg();
        let mut b = Burner::default();
        let demand = Demand { output: 40.0, boost: false };
        // 40 % of a 20 minute cycle: the first 8 minutes.
        assert!(b.update(&c, demand, 0));
        assert!(b.update(&c, demand, 7 * MINUTE + 59_000));
        assert!(!b.update(&c, demand, 8 * MINUTE));
        assert!(!b.update(&c, demand, 19 * MINUTE));
        assert!(b.update(&c, demand, 20 * MINUTE));
    }

    #[test]
    fn burner_minimum_on_and_off_times() {
        let c = cfg();
        let mut b = Burner::default();
        let demand = Demand { output: 10.0, boost: false }; // 2 min of 20
        assert!(b.update(&c, demand, 0));
        assert!(b.update(&c, demand, 2 * MINUTE), "held on for 3 minutes");
        assert!(!b.update(&c, demand, 3 * MINUTE));
        let full = Demand { output: 100.0, boost: false };
        assert!(!b.update(&c, full, 4 * MINUTE), "held off for 5 minutes");
        assert!(b.update(&c, full, 8 * MINUTE));
    }

    #[test]
    fn boost_overrides_minimum_off_time() {
        let c = cfg();
        let mut b = Burner::default();
        b.update(&c, Demand { output: 10.0, boost: false }, 0);
        b.update(&c, Demand { output: 0.0, boost: false }, 3 * MINUTE);
        assert!(b.update(&c, Demand { output: 0.0, boost: true }, 4 * MINUTE));
    }

    #[test]
    fn boost_window() {
        assert!(boost_active(30, 0, 27 * MINUTE));
        assert!(!boost_active(30, 0, 28 * MINUTE));
        assert!(!boost_active(0, 0, 0), "old code underflowed and boosted forever");
        assert!(!boost_active(5, 0, 3 * MINUTE + 1));
    }
}
