//! Pace of the requests to TONE3000, to stay under its rate limiter: a burst
//! is sent at once, then the requests are spread so that no more than
//! `PER_MINUTE` go out in a minute (as PiPedal does). Every API call and
//! model download waits for its turn here.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Requests sent in any minute at most
const PER_MINUTE: usize = 25;
/// Requests sent without waiting
const BURST: usize = 12;
const WINDOW: Duration = Duration::from_secs(60);
/// Gap between requests after the burst: the rest of the minute's allowance
/// spread over the minute
const SPACING: Duration = Duration::from_millis(60_000 / (PER_MINUTE - BURST) as u64);

/// Times of the requests of the last minute
#[derive(Default)]
struct Limiter {
    sent: VecDeque<Instant>,
}

impl Limiter {
    /// How long a request made at `now` must wait
    fn delay(&mut self, now: Instant) -> Duration {
        while self
            .sent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= WINDOW)
        {
            self.sent.pop_front();
        }
        match (self.sent.len(), self.sent.front(), self.sent.back()) {
            (n, Some(first), _) if n >= PER_MINUTE => {
                (*first + WINDOW).saturating_duration_since(now)
            }
            (n, _, Some(last)) if n >= BURST => (*last + SPACING).saturating_duration_since(now),
            _ => Duration::ZERO,
        }
    }
}

static LIMITER: Mutex<Limiter> = Mutex::new(Limiter {
    sent: VecDeque::new(),
});

/// Wait for the turn of one request; false if `cancel` was set meanwhile
pub fn acquire(cancel: Option<&AtomicBool>) -> bool {
    loop {
        let wait = {
            let mut limiter = LIMITER.lock().unwrap();
            let now = Instant::now();
            let wait = limiter.delay(now);
            if wait.is_zero() {
                limiter.sent.push_back(now);
                return true;
            }
            wait
        };
        // In steps, to notice a cancel and let other requests go first
        std::thread::sleep(wait.min(Duration::from_millis(250)));
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Send `count` requests as soon as allowed, from `start`; their times
    fn run(limiter: &mut Limiter, start: Instant, count: usize) -> Vec<Duration> {
        let mut now = start;
        (0..count)
            .map(|_| {
                now += limiter.delay(now);
                limiter.sent.push_back(now);
                now - start
            })
            .collect()
    }

    #[test]
    fn bursts_then_spreads_requests() {
        let mut limiter = Limiter::default();
        let start = Instant::now();
        let times = run(&mut limiter, start, 40);
        assert!(times[..BURST].iter().all(|t| t.is_zero()));
        assert_eq!(times[BURST], SPACING);
        // Never more than PER_MINUTE in any minute
        for (i, t) in times.iter().enumerate() {
            let in_minute = times[i..].iter().filter(|u| **u - *t < WINDOW).count();
            assert!(in_minute <= PER_MINUTE, "{in_minute} requests from {t:?}");
        }
    }

    #[test]
    fn a_quiet_minute_allows_a_new_burst() {
        let mut limiter = Limiter::default();
        let start = Instant::now();
        run(&mut limiter, start, PER_MINUTE);
        let later = start + Duration::from_secs(600);
        assert_eq!(limiter.delay(later), Duration::ZERO);
        assert!(limiter.sent.is_empty());
    }
}
