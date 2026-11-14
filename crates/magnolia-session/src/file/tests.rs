//! The path of a field at an error's line and column, and the example of the module's
//! documentation, which must parse as it stands. The refusals are tested through the loader
//! (`tests/session.rs`).

use std::error::Error;

use super::position::field_at;
use super::{Insert, Instrument, SessionFile, TrackKind, parse};

type TestResult = Result<(), Box<dyn Error>>;

/// The path at the error serde_json reports for `text`, read as a session.
fn path_of_error(text: &str) -> Result<String, Box<dyn Error>> {
    match serde_json::from_str::<SessionFile>(text) {
        Ok(_) => Err("the text reads as a session".into()),
        Err(error) => Ok(field_at(text, error.line(), error.column())),
    }
}

#[test]
fn the_path_names_the_field_whose_value_is_wrong() {
    let text = "{\n  \"tracks\": [\n    {}, {\"clips\": [{\"gain\": \"loud\"}]}\n  ]\n}";
    let error = serde_json::from_str::<serde_json::Value>(text).map(|_| ());
    assert!(error.is_ok(), "the text is JSON");
    // Where serde_json stops reading the gain's string: line 3, after `"loud"`.
    let column = "    {}, {\"clips\": [{\"gain\": \"loud\"".len();
    assert_eq!(field_at(text, 3, column), "tracks[1].clips[0].gain");
}

#[test]
fn the_path_names_an_unknown_field_and_the_object_that_lacks_a_field() -> TestResult {
    let unknown = r#"{"format": "magnolia-test-session", "version": 1, "colour": "red"}"#;
    assert_eq!(path_of_error(unknown)?, "colour");
    let missing = r#"{"format": "magnolia-test-session", "version": 1, "sample_rate": 48000,
        "tempo": {"origin": 0, "changes": []}}"#;
    assert_eq!(path_of_error(missing)?, "tempo");
    Ok(())
}

#[test]
fn the_path_skips_keys_and_brackets_inside_strings() -> TestResult {
    let text = r#"{"buses": [{"name": "a [b], {c}: \"d\"", "channels": "two"}]}"#;
    assert_eq!(path_of_error(text)?, "buses[0].channels");
    Ok(())
}

#[test]
fn the_path_is_empty_at_the_top_level_and_past_the_text() {
    assert_eq!(field_at("", 1, 0), "");
    assert_eq!(field_at("[1, 2", 0, 0), "");
    assert_eq!(field_at("[1, 2", 1, 400), "[1]");
    assert_eq!(field_at("{\"a\": 1}", 7, 3), "");
}

#[test]
fn the_documented_example_parses() -> TestResult {
    let documented = include_str!("mod.rs")
        .lines()
        .skip_while(|line| *line != "//! ```json")
        .skip(1)
        .take_while(|line| *line != "//! ```")
        .map(|line| line.strip_prefix("//! ").unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let file = parse(&documented)?;
    assert_eq!(file.sample_rate, 48_000);
    let [change] = file.tempo.changes.as_slice() else {
        return Err("one tempo change".into());
    };
    assert_eq!(change.bpm, [140, 1]);
    let [bus] = file.buses.as_slice() else {
        return Err("one bus".into());
    };
    assert_eq!(bus.inserts, [Insert::TestLatency { samples: 64 }]);
    let [audio, midi, input] = file.tracks.as_slice() else {
        return Err("three tracks".into());
    };
    assert_eq!((audio.kind, audio.channels), (TrackKind::Audio, Some(1)));
    assert_eq!(midi.kind, TrackKind::Midi);
    assert_eq!(midi.instrument, Some(Instrument::TestSynth {}));
    assert!(midi.clips().iter().all(|clip| clip.file_offset.is_none()));
    assert_eq!((input.kind, input.channels), (TrackKind::Input, Some(1)));
    assert_eq!(input.inputs.as_deref(), Some(&[1][..]));
    assert_eq!(input.armed, Some(true));
    assert!(input.clips.is_none());
    Ok(())
}
