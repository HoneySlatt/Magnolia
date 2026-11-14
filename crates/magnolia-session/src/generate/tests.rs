//! The generator's pieces that the tests of the whole cannot reach cheaply: the reference
//! projects' plans, which hold what `docs/reference-projects.md` says and parse, without writing
//! their gigabytes; the pseudo-random numbers; and the tables the audio is computed from.

use super::audio::Synth;
use super::song::Song;
use super::{Content, Plan, Random, high, lowest, session_file};
use crate::file::{self, Insert, MASTER, TrackKind};

/// The audio tracks of a plan by their channels, mono then stereo, its MIDI tracks, and its input
/// tracks.
fn counts(plan: &Plan) -> (usize, usize, usize, usize) {
    let mut counts = (0, 0, 0, 0);
    for track in &plan.tracks {
        match &track.file {
            Some((_, Content::Audio(part))) if part.channels() == 1 => counts.0 += 1,
            Some((_, Content::Audio(_))) => counts.1 += 1,
            Some((_, Content::Midi(..))) => counts.2 += 1,
            None => counts.3 += 1,
        }
    }
    counts
}

/// The plan's session, written and read back.
fn parses(plan: &Plan, song: &Song) -> file::SessionFile {
    file::parse(&session_file(song, plan).to_json().unwrap()).unwrap()
}

#[test]
fn the_lowest_tiers_project_holds_its_tracks_and_buses() {
    let song = Song::reference().unwrap();
    // 3 minutes 30 seconds at 48 kHz.
    assert_eq!(song.sample_of(song.end()), 210 * 48_000);
    let plan = lowest(&song);
    assert_eq!(counts(&plan), (10, 6, 8, 0));
    let session = parses(&plan, &song);
    let buses: Vec<&str> = session.buses.iter().map(|bus| bus.name.as_str()).collect();
    assert_eq!(buses, ["vocals", "drums", "beat", "room"]);
    assert_eq!(session.buses[1].output, "beat", "a bus into a bus");
    let sends_to_room = session
        .tracks
        .iter()
        .filter(|track| track.sends.iter().any(|send| send.to == "room"))
        .count();
    assert_eq!(sends_to_room, 13, "the vocals and the lead");
    // Vocal takes leave gaps between their clips.
    assert!(session.tracks[0].clips().len() > 1);
}

#[test]
fn the_high_tiers_project_holds_its_tracks() {
    let song = Song::reference().unwrap();
    let plan = high(&song);
    assert_eq!(counts(&plan), (24, 24, 24, 1));
    let session = parses(&plan, &song);
    assert!(
        session
            .buses
            .iter()
            .all(|bus| bus.output == MASTER || bus.output == "beat")
    );
    // Its live voice: mono, from capture channel 1, armed, into the vocals' bus through the gain
    // that holds Auto-Tune's place.
    let voice = session.tracks.last().unwrap();
    assert_eq!(voice.name, "live voice");
    assert_eq!(voice.kind, TrackKind::Input);
    assert_eq!(voice.channels, Some(1));
    assert_eq!(voice.inputs.as_deref(), Some(&[1][..]));
    assert_eq!(voice.armed, Some(true));
    assert_eq!(voice.inserts, [Insert::Gain { value: 1.0 }]);
    assert_eq!(voice.output, "vocals");
    assert!(voice.clips.is_none());
}

#[test]
fn the_numbers_are_splitmix64s() {
    // The first output of SplitMix64 from a state of 0.
    assert_eq!(Random(0).next(), 0xE220_A839_7B1D_CDAF);
    let mut one = Random::for_name("audio/vocal.wav");
    let mut two = Random::for_name("audio/vocal.wav");
    assert_eq!(one.next(), two.next());
    assert_ne!(
        Random::for_name("audio/vocal.wav").next(),
        Random::for_name("audio/double.wav").next()
    );
    for _ in 0..1_000 {
        let unit = one.unit();
        assert!((0.0..1.0).contains(&unit));
    }
}

#[test]
fn the_sine_and_the_pitches_are_exact_enough() {
    let synth = Synth::new(48_000);
    for step in 0..1_000 {
        let cycles = f64::from(step) / 1_000.0;
        // The library's sine only checks the table here; nothing written uses it.
        let exact = (std::f64::consts::TAU * cycles).sin();
        assert!((synth.sine(cycles) - exact).abs() < 1e-6, "{cycles}");
    }
    assert!((synth.frequency(81) - 880.0).abs() < 1e-9);
    assert!((synth.frequency(57) - 220.0).abs() < 1e-9);
}
