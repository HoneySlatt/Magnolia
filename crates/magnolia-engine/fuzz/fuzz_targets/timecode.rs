//! Fuzzes timecode with arbitrary frame rates, labels, clocks and positions, valid or not. Labels
//! must round-trip through their frame numbers, a clock must find the frame of each label it
//! gives, and frames must follow the rounding rule; nothing may panic or overflow.

#![no_main]

#[path = "../../tests/common/mod.rs"]
mod common;

use common::{check_round_trip, check_rule_at};
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use magnolia_engine::time::{
    FramePosition, FrameRate, Grid, SampleRate, TimePosition, TimeUnits, Timecode, TimecodeClock,
};

fuzz_target!(|data: &[u8]| {
    // An input that runs out ends its case early; that is not a failure.
    let _ = check(&mut Unstructured::new(data));
});

fn check(input: &mut Unstructured<'_>) -> Result<()> {
    let rate = *input.choose(&FrameRate::ALL)?;
    let day = rate.frames_per_day();

    let label = Timecode {
        hours: input.arbitrary()?,
        minutes: input.arbitrary()?,
        seconds: input.arbitrary()?,
        frames: input.arbitrary()?,
    };
    if let Some(number) = label.frame_number(rate) {
        assert!((0..day).contains(&number), "{label:?} is outside a day");
        assert_eq!(Timecode::from_frame_number(number, rate), label);
    }

    let number: i64 = input.arbitrary()?;
    let label = Timecode::from_frame_number(number, rate);
    assert_eq!(
        label.frame_number(rate),
        Some(number.rem_euclid(day)),
        "{label:?}"
    );

    let start_label = Timecode::from_frame_number(input.arbitrary()?, rate);
    let clock = TimecodeClock::new(rate, start_label, TimePosition(input.arbitrary()?))
        .expect("a label the rate gives starts a clock");
    let samples =
        SampleRate::new(*input.choose(&SampleRate::STANDARD_HERTZ)?).expect("a standard rate");
    for _ in 0..8 {
        let frame = FramePosition(input.arbitrary()?);
        let found = clock
            .frame_of(clock.label(frame))
            .expect("a label the clock gives");
        assert_eq!(
            (i128::from(found.0) - i128::from(frame.0)).rem_euclid(i128::from(day)),
            0,
            "{frame:?} and {found:?} carry the same label, so lie whole days apart"
        );
        assert!(
            (-day / 2..day - day / 2).contains(&found.0),
            "{found:?} is not the nearest"
        );
        // Samples and time units are each shorter than a frame.
        check_round_trip(frame, &clock, &samples);
        check_round_trip(frame, &clock, &TimeUnits);

        // Any time position lies within the frames' range: a frame is longer than a time unit.
        let time = TimePosition(input.arbitrary()?);
        let moment = TimeUnits
            .moment_of(time)
            .expect("a time position has a moment");
        assert!(
            check_rule_at(&clock, moment),
            "{time:?} is out of the frames' range"
        );
    }
    Ok(())
}
