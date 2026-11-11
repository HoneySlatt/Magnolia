//! The checks of the time model's rounding rule, shared by the integration tests and the fuzz
//! targets, which include this file by its path. A conversion whose result lies beyond its type's
//! range is not checked; each check says whether everything it converted was in range.

use std::fmt::Debug;

use magnolia_engine::time::{
    FramePosition, Grid, Moment, MusicalPosition, SamplePosition, TimePosition, place, read,
};

/// A position that can be stepped to its neighbours.
pub trait Step: Copy {
    /// The position `by` steps away, or `None` beyond the type's range.
    fn step(self, by: i64) -> Option<Self>;
}

impl Step for SamplePosition {
    fn step(self, by: i64) -> Option<Self> {
        self.0.checked_add(by).map(SamplePosition)
    }
}

impl Step for TimePosition {
    fn step(self, by: i64) -> Option<Self> {
        self.0.checked_add(by).map(TimePosition)
    }
}

impl Step for MusicalPosition {
    fn step(self, by: i64) -> Option<Self> {
        self.0.checked_add(by).map(MusicalPosition)
    }
}

impl Step for FramePosition {
    fn step(self, by: i64) -> Option<Self> {
        self.0.checked_add(by).map(FramePosition)
    }
}

/// Checks the rounding rule where `moment` falls on `grid`: placed, it lands on the first position
/// at or after it; read, on the last position at or before it.
pub fn check_rule_at<G: Grid>(grid: &G, moment: Moment) -> bool
where
    G::Position: Step + Debug,
{
    let placed = grid.first_at_or_after(moment);
    if let Some(placed) = placed {
        let at = grid
            .moment_of(placed)
            .expect("a placed position has a moment");
        assert!(at >= moment, "{placed:?} lies before the moment placed");
        if let Some(before) = placed
            .step(-1)
            .and_then(|previous| grid.moment_of(previous))
        {
            assert!(
                before < moment,
                "{placed:?} is not the first at or after the moment"
            );
        }
    }
    let read = grid.last_at_or_before(moment);
    if let Some(read) = read {
        let at = grid.moment_of(read).expect("a read position has a moment");
        assert!(at <= moment, "{read:?} lies after the moment read");
        if let Some(after) = read.step(1).and_then(|next| grid.moment_of(next)) {
            assert!(
                after > moment,
                "{read:?} is not the last at or before the moment"
            );
        }
    }
    placed.is_some() && read.is_some()
}

/// Checks that a position of `coarse` placed on `fine` then read back is itself, and the same the
/// other way round: a grid at least as fine loses nothing.
pub fn check_round_trip<Coarse: Grid, Fine: Grid>(
    position: Coarse::Position,
    coarse: &Coarse,
    fine: &Fine,
) -> bool
where
    Coarse::Position: Debug,
{
    let placed = place(position, coarse, fine);
    if let Some(placed) = placed {
        assert_eq!(
            read(placed, fine, coarse),
            Some(position),
            "placed, then read back"
        );
    }
    let read_back = read(position, coarse, fine);
    if let Some(read_back) = read_back {
        assert_eq!(
            place(read_back, fine, coarse),
            Some(position),
            "read, then placed back"
        );
    }
    placed.is_some() && read_back.is_some()
}
