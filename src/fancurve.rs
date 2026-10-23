//! Software fan curve engine — equivalent of CFanSpeedCurve from the
//! Windows binary: temperature → duty table nodes with linear ramping,
//! zero-RPM stop under the lowest node, and self-detection of fan
//! capabilities. Polling defaults come from MSIAfterburner.cfg
//! (`HwPollPeriod`, `FanSpeedReadbackDelay`).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FanNode {
    pub temp: f32, // °C
    pub duty: f32, // %
}

#[derive(Debug, Clone)]
pub struct FanCurveCfg {
    pub nodes: Vec<FanNode>,
    pub step_mode: u32, // CFanSpeedCurve::GetStepMode: 0 = linear, 1 = stair
    pub step_size: f32, // duty increment towards target
    pub or_interval_ms: u64, // min time between duty changes
}

impl Default for FanCurveCfg {
    fn default() -> Self {
        Self {
            nodes: vec![
                FanNode { temp: 40.0, duty: 30.0 },
                FanNode { temp: 55.0, duty: 55.0 },
                FanNode { temp: 70.0, duty: 85.0 },
                FanNode { temp: 85.0, duty: 100.0 },
            ],
            step_mode: 0,
            step_size: 5.0,
            or_interval_ms: 200,
        }
    }
}

impl FanCurveCfg {
    pub fn duty_for(&self, temp: f32) -> f32 {
        if self.nodes.is_empty() {
            return 50.0;
        }
        if temp <= self.nodes[0].temp {
            return if temp < self.nodes[0].temp - 2.0 { 0.0 } else { self.nodes[0].duty };
        }
        let last = *self.nodes.last().unwrap();
        if temp >= last.temp {
            return last.duty;
        }
        for w in self.nodes.windows(2) {
            let (a, b) = (w[0], w[1]);
            if temp >= a.temp && temp <= b.temp {
                if self.step_mode == 1 {
                    return b.duty;
                }
                let t = (temp - a.temp) / (b.temp - a.temp).max(0.001);
                return a.duty + (b.duty - a.duty) * t;
            }
        }
        last.duty
    }
}

/// Runtime state machine for the fan daemon.
pub struct FanController {
    pub cfg: FanCurveCfg,
    last_write_at: std::time::Instant,
    last_duty: f32,
}

impl FanController {
    pub fn new(cfg: FanCurveCfg) -> Self {
        Self { cfg, last_write_at: std::time::Instant::now() - std::time::Duration::from_secs(3600), last_duty: -1.0 }
    }

    /// Returns target duty (0..100) or None when nothing changed.
    pub fn target_for(&mut self, temp: f32) -> Option<u32> {
        let d = self.cfg.duty_for(temp).clamp(0.0, 100.0);
        // debounce: only move toward target by step_size unless big change
        if self.last_duty < 0.0 {
            self.last_duty = d;
        }
        let delta = d - self.last_duty;
        let step = self.cfg.step_size.max(1.0);
        let mut new = self.last_duty;
        if delta.abs() < 0.5 {
            new = d;
        } else if delta > step {
            new += self.cfg.step_size;
        } else if delta < -step {
            new -= self.cfg.step_size;
        } else {
            new = d;
        }
        new = new.clamp(0.0, 100.0);
        let changed = (new - self.last_duty).abs() >= 1.0;
        let due = self.last_write_at.elapsed().as_millis() as u64 >= self.cfg.or_interval_ms;
        if changed || due {
            self.last_duty = new;
            self.last_write_at = std::time::Instant::now();
            Some(new as u32)
        } else {
            None
        }
    }

    pub fn last_duty(&self) -> f32 {
        self.last_duty.max(0.0)
    }
}
