//! How much may be on the way to a page that shows the Editor: about
//! `TARGET` of data at the speed the page gets it, so a fast link keeps its
//! frame rate and a slow one waits less behind its own messages.
//!
//! The page acknowledges each frame once drawn. Messages arrive in order,
//! so an acknowledgement also tells that the state and meter messages sent
//! before that frame arrived: they count with it. Each acknowledgement
//! gives a delivery rate: the bytes delivered since that frame left, over
//! the time it took (the way TCP BBR measures a link). The estimate is the
//! highest rate of the last `RATE_WINDOW`. A frame sent while the budget
//! was far from full only shows how little there was to send, not what the
//! link can do: its rate counts only when it raises the estimate.
//!
//! The budget also covers the round trip (twice the shortest one seen),
//! measured on small pictures sent while nothing else was on the way: with
//! a queue of our own in front, the round trip would grow with the budget.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Data on the way, in time at the page's delivery rate
const TARGET: Duration = Duration::from_millis(100);
/// Budget bounds: small for a phone on weak Wi-Fi, large enough for a page
/// change in one go on a LAN
pub const MIN_BUDGET: usize = 8 * 1024;
pub const MAX_BUDGET: usize = 256 * 1024;
/// Budget before the first measures
const START_BUDGET: usize = 32 * 1024;
/// Rates are kept this long: a slower link shows after that
const RATE_WINDOW: Duration = Duration::from_secs(2);
/// The shortest round trip is kept this long, unless no new one is seen
const RTT_WINDOW: Duration = Duration::from_secs(10);
/// Round trips are measured on pictures this small at most: a larger one
/// takes long to send on a slow link, which is not the round trip
const RTT_PROBE: usize = 4 * 1024;

/// A picture sent (its frames), not acknowledged yet
#[derive(Debug)]
struct Sent {
    /// The picture and the messages sent just before it
    size: usize,
    /// Its frames not acknowledged yet
    frames: usize,
    /// Bytes delivered, and when the last of them were, as it left
    delivered: usize,
    delivered_at: Instant,
    sent_at: Instant,
    /// When the last picture acknowledged then had left
    first_sent_at: Instant,
    /// Sent while the budget was far from full
    app_limited: bool,
    /// Sent while nothing was on the way, and small: its round trip is
    /// the link's
    probe: bool,
}

#[derive(Debug)]
pub struct FlowControl {
    in_flight: VecDeque<Sent>,
    in_flight_bytes: usize,
    /// Messages sent since the last frame, while frames are on the way
    texts: usize,
    delivered: usize,
    delivered_at: Option<Instant>,
    /// When the last picture acknowledged had left
    acked_sent_at: Option<Instant>,
    /// Rates measured (bytes per second), newest last
    rates: VecDeque<(Instant, f64)>,
    min_rtt: Option<(Duration, Instant)>,
    budget: usize,
}

impl Default for FlowControl {
    fn default() -> Self {
        Self {
            in_flight: VecDeque::new(),
            in_flight_bytes: 0,
            texts: 0,
            delivered: 0,
            delivered_at: None,
            acked_sent_at: None,
            rates: VecDeque::new(),
            min_rtt: None,
            budget: START_BUDGET,
        }
    }
}

impl FlowControl {
    /// Bytes allowed on the way now
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Bytes sent and not known to be delivered
    pub fn in_flight_bytes(&self) -> usize {
        self.in_flight_bytes + self.texts
    }

    /// No frame on the way
    pub fn idle(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Whether a picture of `size` bytes in `frames` frames may go now (one
    /// always may when nothing is on the way), and if so, count it as sent.
    /// A picture goes whole: the areas of a page change arrive together.
    pub fn admit(&mut self, size: usize, frames: usize, now: Instant) -> bool {
        let alone = self.idle();
        if alone {
            // Messages sent since the last frame are not known to have
            // arrived, but nothing tells otherwise either
            self.texts = 0;
        } else if self.in_flight_bytes() + size > self.budget {
            return false;
        }
        let (delivered_at, first_sent_at) = match (self.delivered_at, self.acked_sent_at) {
            (Some(at), Some(sent)) if !alone => (at, sent),
            // The link was idle until now
            _ => (now, now),
        };
        let size = size + std::mem::take(&mut self.texts);
        self.in_flight_bytes += size;
        self.in_flight.push_back(Sent {
            size,
            frames: frames.max(1),
            delivered: self.delivered,
            delivered_at,
            sent_at: now,
            first_sent_at,
            app_limited: self.in_flight_bytes < self.budget / 2,
            probe: alone && size <= RTT_PROBE,
        });
        true
    }

    /// Whether a state or meter message may go now: not while the frames
    /// on the way use the budget (the next state replaces it)
    pub fn text_allowed(&self) -> bool {
        self.idle() || self.in_flight_bytes() < self.budget
    }

    /// A state or meter message of `size` bytes was sent
    pub fn text_sent(&mut self, size: usize) {
        if !self.idle() {
            self.texts += size;
        }
    }

    /// The page drew the oldest frame on the way
    pub fn acked(&mut self, now: Instant) {
        if let Some(sent) = self.in_flight.front_mut() {
            sent.frames -= 1;
            if sent.frames > 0 {
                return;
            }
        }
        let Some(sent) = self.in_flight.pop_front() else {
            return;
        };
        self.in_flight_bytes -= sent.size;
        self.delivered += sent.size;
        self.delivered_at = Some(now);
        self.acked_sent_at = Some(sent.sent_at);

        if sent.probe {
            let rtt = now.saturating_duration_since(sent.sent_at);
            if self.min_rtt.is_none_or(|(min, at)| {
                rtt <= min || now.saturating_duration_since(at) > RTT_WINDOW
            }) {
                self.min_rtt = Some((rtt, now));
            }
        }

        // The longer of the sending and acknowledging times: acknowledgements
        // that arrive bunched up would make the link look faster than it is
        let interval = now
            .saturating_duration_since(sent.delivered_at)
            .max(sent.sent_at.saturating_duration_since(sent.first_sent_at));
        if interval < Duration::from_millis(1) {
            return;
        }
        let rate = (self.delivered - sent.delivered) as f64 / interval.as_secs_f64();
        while self
            .rates
            .front()
            .is_some_and(|&(at, _)| now.saturating_duration_since(at) > RATE_WINDOW)
        {
            self.rates.pop_front();
        }
        if !sent.app_limited || rate > self.rate() {
            self.rates.push_back((now, rate));
        }
        if !self.rates.is_empty() {
            let rtt = self.min_rtt.map_or(Duration::ZERO, |(rtt, _)| rtt * 2);
            let bytes = self.rate() * TARGET.max(rtt).as_secs_f64();
            self.budget = (bytes as usize).clamp(MIN_BUDGET, MAX_BUDGET);
        }
    }

    /// The delivery rate of the link, bytes per second: without recent
    /// measures, the one the budget was made for
    fn rate(&self) -> f64 {
        self.rates
            .iter()
            .map(|&(_, r)| r)
            .reduce(f64::max)
            .unwrap_or(self.budget as f64 / TARGET.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link of `rate` bytes per second and `delay` one way, a sender that
    /// always has frames of `size` bytes and a state message of `text`
    /// bytes before each: the flow control it ends up with
    fn settle(rate: f64, delay: Duration, size: usize, text: usize, seconds: u64) -> FlowControl {
        settle_bunched(rate, delay, size, text, seconds, None)
    }

    /// The same, acknowledgements arriving together every `bunch`
    fn settle_bunched(
        rate: f64,
        delay: Duration,
        size: usize,
        text: usize,
        seconds: u64,
        bunch: Option<Duration>,
    ) -> FlowControl {
        let mut flow = FlowControl::default();
        let start = Instant::now();
        let end = start + Duration::from_secs(seconds);
        let mut now = start;
        // When the link is free, and when each frame on the way is drawn
        let mut link_free = start;
        let mut arrivals: VecDeque<Instant> = VecDeque::new();
        let mut send = |bytes: usize, now: Instant| {
            let begin = link_free.max(now);
            link_free = begin + Duration::from_secs_f64(bytes as f64 / rate);
            link_free + delay * 2
        };
        while now < end {
            let before = flow.in_flight_bytes();
            loop {
                if text > 0 && flow.text_allowed() {
                    send(text, now);
                    flow.text_sent(text);
                }
                if !flow.admit(size, 1, now) {
                    break;
                }
                arrivals.push_back(send(size, now));
            }
            let Some(mut at) = arrivals.pop_front() else {
                break;
            };
            if let Some(bunch) = bunch {
                let n = at
                    .duration_since(start)
                    .as_nanos()
                    .div_ceil(bunch.as_nanos());
                at = start + bunch * n as u32;
            }
            // Never more than the budget on the way (and a message), once
            // the frames sent before a smaller budget are delivered
            let limit = before.max(flow.budget() + text).max(size + text);
            assert!(flow.in_flight_bytes() <= limit);
            now = at;
            flow.acked(now);
        }
        flow
    }

    #[test]
    fn budget_follows_the_link() {
        let ms = Duration::from_millis;
        // About 100 ms of data on a 1 MB/s link
        let budget = settle(1_000_000.0, ms(5), 4000, 0, 5).budget();
        assert!((80_000..=130_000).contains(&budget), "{budget}");
        // A slow link: the floor
        assert_eq!(settle(60_000.0, ms(5), 2000, 0, 10).budget(), MIN_BUDGET);
        // A LAN: the ceiling
        assert_eq!(
            settle(50_000_000.0, ms(1), 16_000, 0, 2).budget(),
            MAX_BUDGET
        );
        // A long round trip needs more than 100 ms of data
        let budget = settle(1_000_000.0, ms(100), 4000, 0, 5).budget();
        assert!(budget >= 200_000, "{budget}");
        // State messages count: the link is the same
        let budget = settle(1_000_000.0, ms(5), 2000, 2000, 5).budget();
        assert!((80_000..=130_000).contains(&budget), "{budget}");
    }

    #[test]
    fn bunched_acknowledgements_do_not_inflate_the_rate() {
        let ms = Duration::from_millis;
        let flow = settle_bunched(60_000.0, ms(5), 1500, 0, 10, Some(ms(250)));
        assert!(flow.rate() < 90_000.0, "{}", flow.rate());
        // Enough for the time between acknowledgements, not more
        assert!(flow.budget() <= 40_000, "{}", flow.budget());
    }

    #[test]
    fn one_frame_always_goes() {
        let now = Instant::now();
        let mut flow = FlowControl::default();
        assert!(flow.text_allowed());
        assert!(flow.admit(MAX_BUDGET * 2, 1, now));
        assert!(!flow.admit(1, 1, now));
        assert!(!flow.text_allowed());
        flow.acked(now + Duration::from_millis(10));
        assert!(flow.idle());
        assert!(flow.text_allowed());
        assert_eq!(flow.in_flight_bytes(), 0);
        // Acknowledgements without frames change nothing
        flow.acked(now + Duration::from_millis(20));
        assert!(flow.idle());
    }

    #[test]
    fn texts_count_with_the_next_frame() {
        let now = Instant::now();
        let mut flow = FlowControl::default();
        // Nothing on the way: not counted
        flow.text_sent(1000);
        assert_eq!(flow.in_flight_bytes(), 0);
        assert!(flow.admit(100, 1, now));
        flow.text_sent(1000);
        assert_eq!(flow.in_flight_bytes(), 1100);
        assert!(flow.admit(100, 1, now));
        assert_eq!(flow.in_flight_bytes(), 1200);
        flow.acked(now);
        assert_eq!(flow.in_flight_bytes(), 1100);
        flow.acked(now);
        assert_eq!(flow.in_flight_bytes(), 0);
    }

    #[test]
    fn a_picture_goes_whole() {
        let now = Instant::now();
        let mut flow = FlowControl::default();
        assert!(flow.admit(START_BUDGET * 3, 3, now));
        flow.acked(now);
        flow.acked(now);
        assert!(!flow.idle(), "one frame of the picture still on the way");
        flow.acked(now);
        assert!(flow.idle());
    }

    #[test]
    fn little_to_send_does_not_shrink_the_budget() {
        let mut flow = FlowControl::default();
        let mut now = Instant::now();
        // Fast link, one small frame now and then
        for _ in 0..200 {
            assert!(flow.admit(500, 1, now));
            now += Duration::from_millis(20);
            flow.acked(now);
            now += Duration::from_millis(30);
        }
        assert_eq!(flow.budget(), START_BUDGET);
    }
}
