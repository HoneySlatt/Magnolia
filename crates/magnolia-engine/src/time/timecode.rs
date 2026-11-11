//! Timecode: frames at a film or video rate, their labels, and the clock that lays them on the
//! absolute timeline.

use super::{
    Grid, Moment, TICKS_PER_QUARTER, TIME_UNITS_PER_SECOND, TimePosition, div_ceil, to_i64,
};

/// A timecode frame rate.
///
/// The 1000/1001 rates (23.976, 29.97 and 59.94) label their frames as if they ran at 24, 30 or
/// 60 frames a second, so their labels fall behind the clock by 0.1 %. Drop-frame skips labels to
/// stay in step: the first two labels of each minute at 29.97 and the first four at 59.94, except
/// in every tenth minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameRate {
    /// 24000/1001 frames a second, labelled as 24.
    Fps23_976,
    /// 24 frames a second.
    Fps24,
    /// 25 frames a second.
    Fps25,
    /// 30000/1001 frames a second, labelled as 30.
    Fps29_97,
    /// 30000/1001 frames a second, labelled as 30, drop-frame.
    Fps29_97Drop,
    /// 30 frames a second.
    Fps30,
    /// 50 frames a second.
    Fps50,
    /// 60000/1001 frames a second, labelled as 60.
    Fps59_94,
    /// 60000/1001 frames a second, labelled as 60, drop-frame.
    Fps59_94Drop,
    /// 60 frames a second.
    Fps60,
}

impl FrameRate {
    /// Every frame rate.
    pub const ALL: [FrameRate; 10] = [
        FrameRate::Fps23_976,
        FrameRate::Fps24,
        FrameRate::Fps25,
        FrameRate::Fps29_97,
        FrameRate::Fps29_97Drop,
        FrameRate::Fps30,
        FrameRate::Fps50,
        FrameRate::Fps59_94,
        FrameRate::Fps59_94Drop,
        FrameRate::Fps60,
    ];

    /// The exact rate, as frames per so many seconds: `(30_000, 1_001)` for 29.97.
    pub fn frames_per_second(self) -> (u32, u32) {
        match self {
            FrameRate::Fps23_976 => (24_000, 1_001),
            FrameRate::Fps24 => (24, 1),
            FrameRate::Fps25 => (25, 1),
            FrameRate::Fps29_97 | FrameRate::Fps29_97Drop => (30_000, 1_001),
            FrameRate::Fps30 => (30, 1),
            FrameRate::Fps50 => (50, 1),
            FrameRate::Fps59_94 | FrameRate::Fps59_94Drop => (60_000, 1_001),
            FrameRate::Fps60 => (60, 1),
        }
    }

    /// The frame labels in one labelled second: 30 for 29.97.
    pub fn labels_per_second(self) -> u8 {
        match self {
            FrameRate::Fps23_976 | FrameRate::Fps24 => 24,
            FrameRate::Fps25 => 25,
            FrameRate::Fps29_97 | FrameRate::Fps29_97Drop | FrameRate::Fps30 => 30,
            FrameRate::Fps50 => 50,
            FrameRate::Fps59_94 | FrameRate::Fps59_94Drop | FrameRate::Fps60 => 60,
        }
    }

    /// Whether the rate skips labels to stay in step with the clock.
    pub fn is_drop_frame(self) -> bool {
        matches!(self, FrameRate::Fps29_97Drop | FrameRate::Fps59_94Drop)
    }

    /// The length of a frame, in time units: a whole number at every rate.
    pub fn frame_duration(self) -> i64 {
        let (frames, seconds) = self.frames_per_second();
        TIME_UNITS_PER_SECOND * i64::from(seconds) / i64::from(frames)
    }

    /// The frames in one labelled day, from 00:00:00:00 to 23:59:59 and its last frame.
    pub fn frames_per_day(self) -> i64 {
        24 * 6 * self.frames_per_ten_minutes()
    }

    /// The labels skipped at the start of each minute but every tenth.
    fn dropped_per_minute(self) -> i64 {
        if self.is_drop_frame() {
            i64::from(self.labels_per_second()) / 30 * 2
        } else {
            0
        }
    }

    fn frames_per_ten_minutes(self) -> i64 {
        600 * i64::from(self.labels_per_second()) - 9 * self.dropped_per_minute()
    }
}

/// A timecode label: hours, minutes, seconds and frames, as a clock shows a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timecode {
    /// From 0 to 23.
    pub hours: u8,
    /// From 0 to 59.
    pub minutes: u8,
    /// From 0 to 59.
    pub seconds: u8,
    /// From 0 to the rate's labels per second, minus one.
    pub frames: u8,
}

impl Timecode {
    /// The frames from 00:00:00:00 to this label, or `None` if it is no label at `rate`: a field
    /// out of range, or a label that drop-frame skips.
    pub fn frame_number(self, rate: FrameRate) -> Option<i64> {
        let labels = rate.labels_per_second();
        let in_range =
            self.hours < 24 && self.minutes < 60 && self.seconds < 60 && self.frames < labels;
        let total_minutes = 60 * i64::from(self.hours) + i64::from(self.minutes);
        let dropped = rate.dropped_per_minute();
        let skipped =
            self.seconds == 0 && total_minutes % 10 != 0 && i64::from(self.frames) < dropped;
        if !in_range || skipped {
            return None;
        }
        let labelled_seconds = 60 * total_minutes + i64::from(self.seconds);
        let skipped_so_far = dropped * (total_minutes - total_minutes / 10);
        Some(labelled_seconds * i64::from(labels) + i64::from(self.frames) - skipped_so_far)
    }

    /// The label of the frame `number` frames after 00:00:00:00, counted around a day: the frame
    /// after 23:59:59 and its last frame is 00:00:00:00 again.
    pub fn from_frame_number(number: i64, rate: FrameRate) -> Timecode {
        let number = number.rem_euclid(rate.frames_per_day());
        let dropped = rate.dropped_per_minute();
        let labels = i64::from(rate.labels_per_second());
        let frames_per_dropped_minute = 60 * labels - dropped;
        let ten_minutes = number / rate.frames_per_ten_minutes();
        let in_ten_minutes = number % rate.frames_per_ten_minutes();
        // Add back the labels skipped before this frame: 9 minutes' worth in each whole ten, and
        // one minute's worth for each dropped minute begun in this ten.
        let mut label_number = number + 9 * dropped * ten_minutes;
        if in_ten_minutes > dropped {
            label_number += dropped * ((in_ten_minutes - dropped) / frames_per_dropped_minute);
        }
        let labelled_seconds = label_number / labels;
        // Each field is below 60, or below the labels per second, so the casts are exact.
        Timecode {
            hours: (labelled_seconds / 3_600) as u8,
            minutes: (labelled_seconds / 60 % 60) as u8,
            seconds: (labelled_seconds % 60) as u8,
            frames: (label_number % labels) as u8,
        }
    }
}

/// A position in timecode frames, counted from a [`TimecodeClock`]'s start frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FramePosition(pub i64);

/// Where timecode lies on the absolute timeline: a frame rate, and the label of the frame that
/// starts at a given time position, such as 00:59:50:00 at the session's start. The start may lie
/// anywhere, between two samples or two frames of another clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimecodeClock {
    rate: FrameRate,
    start: TimePosition,
    start_number: i64,
}

impl TimecodeClock {
    /// A clock at `rate` whose frame labelled `start_label` starts at `start`, or `None` if the
    /// label is no label at `rate`.
    pub fn new(
        rate: FrameRate,
        start_label: Timecode,
        start: TimePosition,
    ) -> Option<TimecodeClock> {
        Some(TimecodeClock {
            rate,
            start,
            start_number: start_label.frame_number(rate)?,
        })
    }

    /// The clock's frame rate.
    pub fn rate(&self) -> FrameRate {
        self.rate
    }

    /// The label of a frame.
    pub fn label(&self, frame: FramePosition) -> Timecode {
        let number = i128::from(self.start_number) + i128::from(frame.0);
        let day = i128::from(self.rate.frames_per_day());
        // Below a day, so the cast is exact.
        Timecode::from_frame_number(number.rem_euclid(day) as i64, self.rate)
    }

    /// The frame that carries a label, or `None` if it is no label at the clock's rate. Labels
    /// come round every day; the frame chosen is the one nearest the start frame, from half a day
    /// before it to just under half a day after.
    pub fn frame_of(&self, label: Timecode) -> Option<FramePosition> {
        let day = self.rate.frames_per_day();
        let after_start = (label.frame_number(self.rate)? - self.start_number).rem_euclid(day);
        let nearest = if after_start < day / 2 {
            after_start
        } else {
            after_start - day
        };
        Some(FramePosition(nearest))
    }

    fn start_moment_units(&self) -> i128 {
        i128::from(self.start.0) * i128::from(TICKS_PER_QUARTER)
    }

    fn moment_units_per_frame(&self) -> i128 {
        i128::from(self.rate.frame_duration()) * i128::from(TICKS_PER_QUARTER)
    }
}

impl Grid for TimecodeClock {
    type Position = FramePosition;

    fn moment_of(&self, frame: FramePosition) -> Option<Moment> {
        // At most 2^63 · 2^35.1 · 2^22.2 = 2^120.3: never overflows.
        let since_start = i128::from(frame.0) * self.moment_units_per_frame();
        Some(Moment(self.start_moment_units() + since_start))
    }

    fn first_at_or_after(&self, moment: Moment) -> Option<FramePosition> {
        let since_start = moment.0.checked_sub(self.start_moment_units())?;
        to_i64(div_ceil(since_start, self.moment_units_per_frame())).map(FramePosition)
    }

    fn last_at_or_before(&self, moment: Moment) -> Option<FramePosition> {
        let since_start = moment.0.checked_sub(self.start_moment_units())?;
        to_i64(since_start.div_euclid(self.moment_units_per_frame())).map(FramePosition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{SamplePosition, SampleRate, TimeUnits, place, read};

    fn tc(hours: u8, minutes: u8, seconds: u8, frames: u8) -> Timecode {
        Timecode {
            hours,
            minutes,
            seconds,
            frames,
        }
    }

    #[test]
    fn every_label_of_a_day_round_trips_through_its_frame_number() {
        for rate in FrameRate::ALL {
            let mut previous = None;
            for number in 0..rate.frames_per_day() {
                let label = Timecode::from_frame_number(number, rate);
                assert_eq!(label.frame_number(rate), Some(number), "{rate:?} {label:?}");
                assert!(previous < Some(label), "{rate:?}: labels must increase");
                previous = Some(label);
            }
        }
    }

    #[test]
    fn a_day_holds_the_known_number_of_frames() {
        assert_eq!(FrameRate::Fps24.frames_per_day(), 2_073_600);
        assert_eq!(FrameRate::Fps29_97.frames_per_day(), 2_592_000);
        assert_eq!(FrameRate::Fps29_97Drop.frames_per_day(), 2_589_408);
        assert_eq!(FrameRate::Fps59_94Drop.frames_per_day(), 5_178_816);
    }

    #[test]
    fn drop_frame_skips_the_first_labels_of_each_minute_but_every_tenth() {
        let rate = FrameRate::Fps29_97Drop;
        assert_eq!(tc(0, 1, 0, 0).frame_number(rate), None);
        assert_eq!(tc(0, 1, 0, 1).frame_number(rate), None);
        assert_eq!(tc(0, 1, 0, 2).frame_number(rate), Some(1_800));
        assert_eq!(tc(0, 0, 59, 29).frame_number(rate), Some(1_799));
        assert_eq!(tc(0, 10, 0, 0).frame_number(rate), Some(17_982));
        assert_eq!(tc(10, 0, 0, 0).frame_number(rate), Some(1_078_920));
        let rate = FrameRate::Fps59_94Drop;
        assert_eq!(tc(0, 1, 0, 3).frame_number(rate), None);
        assert_eq!(tc(0, 1, 0, 4).frame_number(rate), Some(3_600));
        assert_eq!(tc(0, 10, 0, 0).frame_number(rate), Some(35_964));
    }

    #[test]
    fn labels_out_of_range_are_refused() {
        let rate = FrameRate::Fps25;
        for label in [
            tc(24, 0, 0, 0),
            tc(0, 60, 0, 0),
            tc(0, 0, 60, 0),
            tc(0, 0, 0, 25),
        ] {
            assert_eq!(label.frame_number(rate), None, "{label:?}");
        }
        assert_eq!(
            tc(23, 59, 59, 24).frame_number(rate),
            Some(rate.frames_per_day() - 1)
        );
    }

    #[test]
    fn every_frame_is_a_whole_number_of_time_units() {
        for rate in FrameRate::ALL {
            let (frames, seconds) = rate.frames_per_second();
            let duration = i128::from(rate.frame_duration());
            assert_eq!(
                duration * i128::from(frames),
                i128::from(TIME_UNITS_PER_SECOND) * i128::from(seconds),
                "{rate:?}"
            );
        }
    }

    #[test]
    fn a_session_starting_before_the_hour_reads_its_labels() {
        let start = tc(0, 59, 50, 0);
        let clock = TimecodeClock::new(FrameRate::Fps25, start, TimePosition(0)).unwrap();
        assert_eq!(clock.label(FramePosition(0)), start);
        assert_eq!(clock.frame_of(tc(1, 0, 0, 0)), Some(FramePosition(250)));
        assert_eq!(clock.frame_of(tc(0, 59, 49, 24)), Some(FramePosition(-1)));
        let rate = SampleRate::new(48_000).unwrap();
        assert_eq!(
            place(FramePosition(250), &clock, &rate),
            Some(SamplePosition(480_000))
        );
    }

    #[test]
    fn a_label_after_midnight_is_the_next_day_near_a_late_start() {
        let clock = TimecodeClock::new(FrameRate::Fps24, tc(23, 0, 0, 0), TimePosition(0)).unwrap();
        assert_eq!(
            clock.frame_of(tc(1, 0, 0, 0)),
            Some(FramePosition(2 * 3_600 * 24))
        );
        assert_eq!(clock.label(FramePosition(2 * 3_600 * 24)), tc(1, 0, 0, 0));
        assert_eq!(
            clock.frame_of(tc(22, 0, 0, 0)),
            Some(FramePosition(-3_600 * 24))
        );
    }

    #[test]
    fn a_frame_starts_on_the_first_sample_at_or_after_it_and_holds_the_samples_until_the_next() {
        let clock =
            TimecodeClock::new(FrameRate::Fps29_97, tc(0, 0, 0, 0), TimePosition(0)).unwrap();
        let rate = SampleRate::new(48_000).unwrap();
        // A frame at 29.97 lasts 1,601.6 samples at 48 kHz: frame 2 starts at 3,203.2.
        assert_eq!(
            place(FramePosition(2), &clock, &rate),
            Some(SamplePosition(3_204))
        );
        assert_eq!(
            read(SamplePosition(3_203), &rate, &clock),
            Some(FramePosition(1))
        );
        assert_eq!(
            read(SamplePosition(3_204), &rate, &clock),
            Some(FramePosition(2))
        );
        let time = place(FramePosition(2), &clock, &TimeUnits).unwrap();
        assert_eq!(read(time, &TimeUnits, &clock), Some(FramePosition(2)));
    }

    #[test]
    fn ten_hours_of_drop_frame_labels_run_ahead_of_the_clock_by_a_frame() {
        let rate = FrameRate::Fps29_97Drop;
        let clock = TimecodeClock::new(rate, tc(0, 0, 0, 0), TimePosition(0)).unwrap();
        let ten_hours = clock.frame_of(tc(10, 0, 0, 0)).unwrap();
        let time = place(ten_hours, &clock, &TimeUnits).unwrap();
        let ten_hours_of_clock = 36_000 * TIME_UNITS_PER_SECOND;
        // 10:00:00;00 is 1,078,920 frames of 1001/30000 s: 35,999.964 s, so the label comes
        // 1.08 frames before ten hours of clock time.
        assert_eq!(time.0, 1_078_920 * rate.frame_duration());
        assert!(ten_hours_of_clock - time.0 > rate.frame_duration());
        assert!(ten_hours_of_clock - time.0 < 2 * rate.frame_duration());
    }

    #[test]
    fn a_label_half_a_day_away_is_the_one_before_the_start() {
        let clock = TimecodeClock::new(FrameRate::Fps24, tc(0, 0, 0, 0), TimePosition(0)).unwrap();
        let half_a_day = FrameRate::Fps24.frames_per_day() / 2;
        assert_eq!(
            clock.frame_of(tc(12, 0, 0, 0)),
            Some(FramePosition(-half_a_day))
        );
        assert_eq!(
            clock.frame_of(tc(11, 59, 59, 23)),
            Some(FramePosition(half_a_day - 1))
        );
    }

    #[test]
    fn an_invalid_start_label_makes_no_clock() {
        let skipped = tc(0, 1, 0, 0);
        assert_eq!(
            TimecodeClock::new(FrameRate::Fps29_97Drop, skipped, TimePosition(0)),
            None
        );
    }
}
