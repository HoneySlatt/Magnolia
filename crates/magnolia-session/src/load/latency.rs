//! How late a session's paths run, from its test latencies: what the memory budget's delay lines
//! and the monitoring path's latency are both worked out from.

use crate::file::{Insert, Strip, Target};

/// The samples of delay a chain of inserts holds: its test latencies'.
pub(crate) fn latency_samples(inserts: &[Insert]) -> u64 {
    inserts
        .iter()
        .map(|insert| match insert {
            Insert::TestLatency { samples } => u64::from(*samples),
            Insert::Gain { .. } => 0,
        })
        .fold(0, u64::saturating_add)
}

/// How far ahead of the output the input of the master and of each bus runs: the latency of its
/// own chain and of every chain after it, to the playback channels. It is also how late a signal
/// that enters there reaches the output when nothing delays it, as on a live path, which the engine
/// never delays to line it up with others (`docs/ARCHITECTURE.md`, *Delay compensation*).
pub(crate) struct Demands {
    master: u64,
    buses: Vec<u64>,
}

impl Demands {
    /// The demands of a session whose master holds `master_inserts`, and whose buses are `buses`.
    pub(crate) fn new(master_inserts: &[Insert], buses: &[Strip]) -> Self {
        let master = latency_samples(master_inserts);
        Self {
            master,
            buses: bus_demands(master, buses),
        }
    }

    /// How far ahead of the output `target`'s input runs.
    pub(crate) fn of(&self, target: Target) -> u64 {
        match target {
            Target::Master => self.master,
            Target::Bus(index) => self.buses.get(index).copied().unwrap_or(self.master),
        }
    }

    /// How late a track's strip, from its first insert, reaches the playback channels at the
    /// latest: its inserts' latency, then the slowest of its output and its sends.
    pub(crate) fn slowest_path(&self, strip: &Strip) -> u64 {
        let after = strip
            .sends
            .iter()
            .map(|&(to, _)| self.of(to))
            .fold(self.of(strip.output), u64::max);
        latency_samples(&strip.inserts).saturating_add(after)
    }
}

/// How far ahead of the output each bus's input runs: the latency of its own chain and of every
/// chain after it, to the master's, which runs `master_demand` ahead.
fn bus_demands(master_demand: u64, buses: &[Strip]) -> Vec<u64> {
    let mut demands: Vec<Option<u64>> = vec![None; buses.len()];
    for start in 0..buses.len() {
        // The chain of buses from `start` to the first whose demand is known, or the master.
        let mut path = Vec::new();
        let mut next = Target::Bus(start);
        let base = loop {
            match next {
                Target::Master => break master_demand,
                Target::Bus(index) => match demands.get(index) {
                    Some(Some(known)) => break *known,
                    // The routing has no cycle; a path longer than the buses would be one.
                    Some(None) if path.len() <= buses.len() => {
                        path.push(index);
                        next = buses.get(index).map_or(Target::Master, |bus| bus.output);
                    }
                    _ => break master_demand,
                },
            }
        };
        let mut demand = base;
        for &index in path.iter().rev() {
            let latency = buses
                .get(index)
                .map_or(0, |bus| latency_samples(&bus.inserts));
            demand = demand.saturating_add(latency);
            if let Some(slot) = demands.get_mut(index) {
                *slot = Some(demand);
            }
        }
    }
    demands.into_iter().map(Option::unwrap_or_default).collect()
}
