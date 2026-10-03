//! `CClock` (0x52CD90..0x52D150): the game clock, 1 game minute per 1000 ms of game time.

/// 0x8CCF24 (February has 29 days).
const DAYS_IN_MONTH: [u8; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

#[derive(Debug, Clone)]
pub struct Clock {
    pub ms_per_minute: u32,
    pub last_tick: u32,
    /// Derived each update; can briefly exceed 59 when catching up.
    pub seconds: u16,
    pub minutes: u8,
    pub hours: u8,
    /// Day of month and month, 1-based.
    pub days: u8,
    pub month: u8,
    /// Day of the week, 1..7.
    pub current_day: u8,
}

impl Clock {
    /// `CClock::Initialise(1000)` as called by `CGame::Init`: 12:00, day 1, month 1.
    pub fn new(now: u32) -> Self {
        Self { ms_per_minute: 1000, last_tick: now, seconds: 0, minutes: 0, hours: 12, days: 1, month: 1, current_day: 4 }
    }

    /// `CClock::Update` (0x52CF10): at most one game minute per call.
    pub fn update(&mut self, now: u32) {
        let elapsed = now.wrapping_sub(self.last_tick);
        if elapsed as i32 > self.ms_per_minute as i32 {
            self.minutes += 1;
            self.last_tick = self.last_tick.wrapping_add(self.ms_per_minute);
            if self.minutes >= 60 {
                self.minutes = 0;
                self.hours += 1;
                if self.hours >= 24 {
                    self.hours = 0;
                    self.days += 1;
                    self.current_day = if self.current_day == 7 { 1 } else { self.current_day + 1 };
                    // `>=`: the last day of each month is never shown (original behaviour).
                    if self.days >= DAYS_IN_MONTH[(self.month - 1) as usize] {
                        self.month += 1;
                        self.days = 1;
                        if self.month > 12 {
                            self.month = 1;
                        }
                    }
                }
            }
        }
        self.seconds = (now.wrapping_sub(self.last_tick).wrapping_mul(60) / self.ms_per_minute) as u16;
    }

    /// `CClock::SetGameClock(h, m, 0)` (0x52D150).
    pub fn set(&mut self, now: u32, hours: u8, minutes: u8) {
        self.last_tick = now;
        self.seconds = 0;
        self.minutes = minutes % 60;
        self.hours = (hours + minutes / 60) % 24;
    }

    /// `GetIsTimeInRange` (0x52CEE0), hours only.
    pub fn is_time_in_range(&self, from: u8, to: u8) -> bool {
        if from > to { self.hours >= from || self.hours < to } else { self.hours >= from && self.hours < to }
    }

    /// `CCustomBuildingDNPipeline::UpdateDNBalance` (0x5D7F80): 1 at night, 0 by day.
    pub fn dn_balance(&self) -> f32 {
        let m = (self.hours as u32 * 60 + self.minutes as u32) as f32 + self.seconds as f32 * (1.0 / 60.0);
        if m < 360.0 {
            1.0
        } else if m < 420.0 {
            (420.0 - m) * (1.0 / 60.0)
        } else if m < 1200.0 {
            0.0
        } else if m < 1260.0 {
            1.0 - (1260.0 - m) * (1.0 / 60.0)
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_minute_is_a_second() {
        let mut c = Clock::new(0);
        for t in (0..=61_000).step_by(20) {
            c.update(t);
        }
        assert_eq!((c.hours, c.minutes), (13, 0));
        let at = |h, m| {
            let mut c = Clock::new(0);
            c.set(0, h, m);
            c.dn_balance()
        };
        assert_eq!(at(3, 0), 1.0);
        assert_eq!(at(6, 30), 0.5);
        assert_eq!(at(12, 0), 0.0);
        assert_eq!(at(20, 30), 0.5);
    }
}
