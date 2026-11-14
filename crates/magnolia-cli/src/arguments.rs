//! The command line's arguments, read by hand: the command, its options, and a plain error for
//! every misuse.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;

use magnolia_engine::node::MAX_LATENCY;
use magnolia_engine::record::TakeFormat;
use magnolia_engine::time::{TIME_UNITS_PER_SECOND, TimePosition};

/// The usage, printed by `--help` and after a misuse.
pub const USAGE: &str = "\
Usage:
  magnolia play <session.json> [--server NAME] [--connect] [--from SECONDS] [--seconds N]
  magnolia record <session.json> <folder> [--server NAME] [--connect] [--from SECONDS]
                  [--seconds N] [--extra-latency FRAMES] [--format float32|int24]
  magnolia recover <folder>
  magnolia render <session.json> <out.wav> [--block FRAMES]
  magnolia --help

play     Plays a test session through JACK, from --from for --seconds, or to the session's end
         and one second of tail; stops, then prints the engine's figures. A session with input
         tracks monitors them, and prints the round trip's latency first.
  --server NAME    the JACK server to play on; the default one otherwise. Magnolia never starts
                   a server.
  --connect        connects the playback ports to the server's first physical playback ports;
                   with record, the capture ports to its first physical capture ports too.
  --from SECONDS   where the play starts on the timeline; 0 by default.
  --seconds N      how long it plays.

record   Plays a test session as play does, and records each armed input track into a take, a
         WAV file in <folder>, an existing folder, which it recovers first. Prints the round
         trip's latency, then the takes and the recorder's figures. Exits with 1 when a take's
         writing failed, or a change of latency ended it. It takes play's options, and:
  --extra-latency FRAMES  the loopback latency jack_iodelay measured beyond the server's, by
                   which takes are placed earlier; 0 by default, 65536 at most.
  --format F       float32, the device's samples bit for bit, by default; or int24.

recover  Repairs the takes a crash left in a recording folder, and lists them. A folder that a
         recording uses is refused.

render   Renders a test session offline, from its start to its end and one second of tail, into
         a new WAV file of 32-bit floats. It never writes over a file.
  --block FRAMES   the frames of each block the engine processes; 4096 by default.

Seconds are digits with an optional fraction: 2, 0.5, 12.25.
";

/// The frames of a render's block when `--block` is not given.
pub const DEFAULT_BLOCK: usize = 4_096;

/// The most decimals a number of seconds may have: a fraction of 18 digits, times the time units
/// in a second, fits a `u128`.
const MOST_DECIMALS: usize = 18;

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Print the usage.
    Help,
    /// Play a session through JACK.
    Play(PlayOptions),
    /// Play a session through JACK and record its armed input tracks.
    Record(RecordOptions),
    /// Recover a recording folder.
    Recover(RecoverOptions),
    /// Render a session offline into a WAV file.
    Render(RenderOptions),
}

/// What `magnolia play` plays, and where.
#[derive(Debug, PartialEq, Eq)]
pub struct PlayOptions {
    /// The session file.
    pub session: PathBuf,
    /// The JACK server to play on, by name; `None` for the default one.
    pub server: Option<String>,
    /// Whether to connect the playback ports to the server's physical playback ports, and, for
    /// `record`, the capture ports to its physical capture ports.
    pub connect: bool,
    /// Where the play starts on the timeline.
    pub from: TimePosition,
    /// How long it plays, in time units; `None` to the session's end and its tail.
    pub length: Option<TimePosition>,
}

/// What `magnolia record` plays and records, and where.
#[derive(Debug, PartialEq, Eq)]
pub struct RecordOptions {
    /// The session, the server and the span, as `play` takes them.
    pub play: PlayOptions,
    /// The recording folder.
    pub folder: PathBuf,
    /// The recorder's latency beyond the server's, in frames.
    pub extra_latency: u32,
    /// How the takes' samples are written.
    pub format: TakeFormat,
}

/// What `magnolia recover` recovers.
#[derive(Debug, PartialEq, Eq)]
pub struct RecoverOptions {
    /// The recording folder.
    pub folder: PathBuf,
}

/// What `magnolia render` renders, and into what.
#[derive(Debug, PartialEq, Eq)]
pub struct RenderOptions {
    /// The session file.
    pub session: PathBuf,
    /// The WAV file to create.
    pub out: PathBuf,
    /// The frames of each block the engine processes.
    pub block: usize,
}

/// A misuse of the command line, said plainly.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(String);

impl fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reads the arguments that follow the program's name. `--help` or `-h` anywhere asks for the
/// usage.
pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let arguments: Vec<OsString> = arguments.into_iter().collect();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        return Ok(Command::Help);
    }
    let Some((command, rest)) = arguments.split_first() else {
        return Err(UsageError("no command given".to_owned()));
    };
    match command.to_str() {
        Some("play") => parse_play(rest).map(Command::Play),
        Some("record") => parse_record(rest).map(Command::Record),
        Some("recover") => parse_recover(rest).map(Command::Recover),
        Some("render") => parse_render(rest).map(Command::Render),
        Some("help") => Ok(Command::Help),
        _ => Err(UsageError(format!("unknown command {command:?}"))),
    }
}

fn parse_play(arguments: &[OsString]) -> Result<PlayOptions, UsageError> {
    let mut play = PlayArguments::default();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(option) = option_name(argument)? else {
            if play.session.is_some() {
                return Err(one_too_many("play takes one session file", argument));
            }
            play.session = Some(PathBuf::from(argument));
            continue;
        };
        if !play.option(option, &mut arguments)? {
            return Err(unknown_option("play", option));
        }
    }
    play.options("play needs a session file")
}

fn parse_record(arguments: &[OsString]) -> Result<RecordOptions, UsageError> {
    let mut play = PlayArguments::default();
    let mut folder = None;
    let mut extra_latency = None;
    let mut format = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(option) = option_name(argument)? else {
            if play.session.is_none() {
                play.session = Some(PathBuf::from(argument));
            } else if folder.is_none() {
                folder = Some(PathBuf::from(argument));
            } else {
                return Err(one_too_many(
                    "record takes a session file and a recording folder",
                    argument,
                ));
            }
            continue;
        };
        match option {
            "--extra-latency" => {
                let text = value(&mut arguments, option)?;
                // A converter adds tens of frames; past a node's largest latency, 1.4 s at
                // 48 kHz, the figure is a mistake, and `record` would wait that long for a take.
                let frames = u32::try_from(frames(option, text)?)
                    .ok()
                    .filter(|&frames| frames <= MAX_LATENCY)
                    .ok_or_else(|| {
                        UsageError(format!(
                            "{option} takes at most {MAX_LATENCY} frames, not {text}"
                        ))
                    })?;
                set_once(&mut extra_latency, option, frames)?;
            }
            "--format" => {
                let take_format = match value(&mut arguments, option)? {
                    "float32" => TakeFormat::Float32,
                    "int24" => TakeFormat::Int24,
                    other => {
                        return Err(UsageError(format!(
                            "{option} takes float32 or int24, not {other:?}"
                        )));
                    }
                };
                set_once(&mut format, option, take_format)?;
            }
            _ if play.option(option, &mut arguments)? => {}
            _ => return Err(unknown_option("record", option)),
        }
    }
    let needs = "record needs a session file and a recording folder";
    let play = play.options(needs)?;
    Ok(RecordOptions {
        play,
        folder: folder.ok_or_else(|| UsageError(needs.to_owned()))?,
        extra_latency: extra_latency.unwrap_or(0),
        format: format.unwrap_or_default(),
    })
}

fn parse_recover(arguments: &[OsString]) -> Result<RecoverOptions, UsageError> {
    let mut folder = None;
    for argument in arguments {
        if let Some(option) = option_name(argument)? {
            return Err(unknown_option("recover", option));
        }
        if folder.is_some() {
            return Err(one_too_many("recover takes one recording folder", argument));
        }
        folder = Some(PathBuf::from(argument));
    }
    Ok(RecoverOptions {
        folder: folder.ok_or_else(|| UsageError("recover needs a recording folder".to_owned()))?,
    })
}

/// The options `play` takes, and `record` with it, as they are read.
#[derive(Default)]
struct PlayArguments {
    session: Option<PathBuf>,
    server: Option<String>,
    connect: Option<()>,
    from: Option<TimePosition>,
    length: Option<TimePosition>,
}

impl PlayArguments {
    /// Reads `option`, and its value from `arguments`, if it is one of `play`'s; returns whether
    /// it was.
    fn option<'a>(
        &mut self,
        option: &str,
        arguments: &mut impl Iterator<Item = &'a OsString>,
    ) -> Result<bool, UsageError> {
        match option {
            "--server" => {
                let name = value(arguments, option)?;
                if name.is_empty() {
                    return Err(UsageError(
                        "--server needs a server's name, not an empty one".to_owned(),
                    ));
                }
                set_once(&mut self.server, option, name.to_owned())?;
            }
            "--connect" => set_once(&mut self.connect, option, ())?,
            "--from" => {
                let start = seconds(option, value(arguments, option)?)?;
                set_once(&mut self.from, option, start)?;
            }
            "--seconds" => {
                let seconds = seconds(option, value(arguments, option)?)?;
                if seconds.0 == 0 {
                    return Err(UsageError(
                        "--seconds takes a number of seconds above 0".to_owned(),
                    ));
                }
                set_once(&mut self.length, option, seconds)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The options read, with their defaults; `needs` says what is missing without a session.
    fn options(self, needs: &str) -> Result<PlayOptions, UsageError> {
        Ok(PlayOptions {
            session: self.session.ok_or_else(|| UsageError(needs.to_owned()))?,
            server: self.server,
            connect: self.connect.is_some(),
            from: self.from.unwrap_or(TimePosition(0)),
            length: self.length,
        })
    }
}

fn parse_render(arguments: &[OsString]) -> Result<RenderOptions, UsageError> {
    let mut paths = Vec::with_capacity(2);
    let mut block = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(option) = option_name(argument)? else {
            if paths.len() == 2 {
                return Err(one_too_many(
                    "render takes a session file and a WAV file",
                    argument,
                ));
            }
            paths.push(PathBuf::from(argument));
            continue;
        };
        match option {
            "--block" => {
                let length = frames(option, value(&mut arguments, option)?)?;
                set_once(&mut block, option, length)?;
            }
            _ => return Err(unknown_option("render", option)),
        }
    }
    let Ok([session, out]) = <[PathBuf; 2]>::try_from(paths) else {
        return Err(UsageError(
            "render needs a session file and the WAV file to write".to_owned(),
        ));
    };
    Ok(RenderOptions {
        session,
        out,
        block: block.unwrap_or(DEFAULT_BLOCK),
    })
}

/// The option an argument names, when it starts with `-`; `None` for a positional argument.
fn option_name(argument: &OsStr) -> Result<Option<&str>, UsageError> {
    if !argument.as_encoded_bytes().starts_with(b"-") {
        return Ok(None);
    }
    argument
        .to_str()
        .map(Some)
        .ok_or_else(|| UsageError(format!("unknown option {argument:?}")))
}

/// The value that follows `option`: present, not itself an option, and valid UTF-8.
fn value<'a>(
    arguments: &mut impl Iterator<Item = &'a OsString>,
    option: &str,
) -> Result<&'a str, UsageError> {
    let Some(value) = arguments.next() else {
        return Err(UsageError(format!("{option} needs a value")));
    };
    if value.as_encoded_bytes().starts_with(b"--") {
        return Err(UsageError(format!(
            "{option} needs a value, not the option {value:?}"
        )));
    }
    value
        .to_str()
        .ok_or_else(|| UsageError(format!("{option} takes text, not {value:?}")))
}

/// Fills `slot` with the value of `option`, given once at most.
fn set_once<T>(slot: &mut Option<T>, option: &str, value: T) -> Result<(), UsageError> {
    if slot.is_some() {
        return Err(UsageError(format!("{option} is given twice")));
    }
    *slot = Some(value);
    Ok(())
}

fn unknown_option(command: &str, option: &str) -> UsageError {
    UsageError(format!("{command} has no option {option:?}"))
}

fn one_too_many(rule: &str, argument: &OsStr) -> UsageError {
    UsageError(format!("{rule}; {argument:?} is one too many"))
}

/// Reads a number of seconds written as digits with an optional fraction (`2`, `0.5`, `.25`),
/// exactly, into time units. A fraction finer than a time unit, which is about a picosecond, is
/// placed on the time unit after it, as `place` places a moment on a grid.
fn seconds(option: &str, text: &str) -> Result<TimePosition, UsageError> {
    let wrong = || {
        UsageError(format!(
            "{option} takes a number of seconds such as 2 or 1.5, not {text:?}"
        ))
    };
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty()) || !digits(whole) || !digits(fraction) {
        return Err(wrong());
    }
    if fraction.len() > MOST_DECIMALS {
        return Err(UsageError(format!(
            "{option} takes at most {MOST_DECIMALS} decimals, not {}",
            fraction.len()
        )));
    }
    let units_per_second = TIME_UNITS_PER_SECOND.unsigned_abs();
    let too_long = || {
        UsageError(format!(
            "{option} takes at most {} seconds, about 118 days, not {text}",
            i64::MAX / TIME_UNITS_PER_SECOND
        ))
    };
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| too_long())?
    };
    let fraction_units = if fraction.is_empty() {
        0
    } else {
        // At most 10^18 · 2^39.8: within a u128.
        let numerator: u128 = fraction.parse().map_err(|_| wrong())?;
        let denominator = 10_u128.pow(fraction.len() as u32);
        (numerator * u128::from(units_per_second)).div_ceil(denominator)
    };
    whole
        .checked_mul(u128::from(units_per_second))
        .and_then(|units| units.checked_add(fraction_units))
        .and_then(|units| i64::try_from(units).ok())
        .map(TimePosition)
        .ok_or_else(too_long)
}

/// Reads a whole number of frames.
fn frames(option: &str, text: &str) -> Result<usize, UsageError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(UsageError(format!(
            "{option} takes a whole number of frames, not {text:?}"
        )));
    }
    text.parse()
        .map_err(|_| UsageError(format!("{option} takes fewer frames than {text}")))
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    const SECOND: i64 = TIME_UNITS_PER_SECOND;

    fn parsed(arguments: &[&str]) -> Result<Command, UsageError> {
        parse(arguments.iter().map(OsString::from))
    }

    /// The message of a misuse.
    fn refused(arguments: &[&str]) -> String {
        match parsed(arguments) {
            Err(error) => error.to_string(),
            Ok(command) => panic!("{arguments:?} was taken as {command:?}"),
        }
    }

    fn play(arguments: &[&str]) -> PlayOptions {
        match parsed(arguments) {
            Ok(Command::Play(options)) => options,
            other => panic!("{arguments:?} gave {other:?}"),
        }
    }

    fn render(arguments: &[&str]) -> RenderOptions {
        match parsed(arguments) {
            Ok(Command::Render(options)) => options,
            other => panic!("{arguments:?} gave {other:?}"),
        }
    }

    #[test]
    fn play_takes_a_session_and_its_defaults() {
        assert_eq!(
            play(&["play", "session.json"]),
            PlayOptions {
                session: PathBuf::from("session.json"),
                server: None,
                connect: false,
                from: TimePosition(0),
                length: None,
            }
        );
    }

    #[test]
    fn play_takes_each_option_in_any_order() {
        let options = play(&[
            "play",
            "--seconds",
            "3",
            "--server",
            "studio",
            "session.json",
            "--connect",
            "--from",
            "1.5",
        ]);
        assert_eq!(
            options,
            PlayOptions {
                session: PathBuf::from("session.json"),
                server: Some("studio".to_owned()),
                connect: true,
                from: TimePosition(SECOND * 3 / 2),
                length: Some(TimePosition(3 * SECOND)),
            }
        );
    }

    #[test]
    fn record_takes_a_session_a_folder_and_plays_options() {
        let Ok(Command::Record(options)) = parsed(&["record", "session.json", "takes"]) else {
            panic!("record was not read");
        };
        assert_eq!(
            options,
            RecordOptions {
                play: play(&["play", "session.json"]),
                folder: PathBuf::from("takes"),
                extra_latency: 0,
                format: TakeFormat::Float32,
            }
        );
        let Ok(Command::Record(options)) = parsed(&[
            "record",
            "session.json",
            "takes",
            "--extra-latency",
            "65536",
        ]) else {
            panic!("record was not read");
        };
        assert_eq!(options.extra_latency, MAX_LATENCY, "the largest taken");
        let Ok(Command::Record(options)) = parsed(&[
            "record",
            "--format",
            "int24",
            "session.json",
            "--from",
            "1.5",
            "takes",
            "--extra-latency",
            "48",
            "--connect",
            "--server",
            "studio",
            "--seconds",
            "3",
        ]) else {
            panic!("record was not read");
        };
        assert_eq!(
            options,
            RecordOptions {
                play: PlayOptions {
                    session: PathBuf::from("session.json"),
                    server: Some("studio".to_owned()),
                    connect: true,
                    from: TimePosition(SECOND * 3 / 2),
                    length: Some(TimePosition(3 * SECOND)),
                },
                folder: PathBuf::from("takes"),
                extra_latency: 48,
                format: TakeFormat::Int24,
            }
        );
    }

    #[test]
    fn recover_takes_a_folder() {
        assert_eq!(
            parsed(&["recover", "takes"]),
            Ok(Command::Recover(RecoverOptions {
                folder: PathBuf::from("takes")
            }))
        );
    }

    #[test]
    fn render_takes_two_files_and_a_block() {
        assert_eq!(
            render(&["render", "session.json", "out.wav"]),
            RenderOptions {
                session: PathBuf::from("session.json"),
                out: PathBuf::from("out.wav"),
                block: DEFAULT_BLOCK,
            }
        );
        assert_eq!(
            render(&["render", "--block", "64", "session.json", "out.wav"]).block,
            64
        );
    }

    #[test]
    fn help_is_asked_in_every_way() {
        for arguments in [
            &["--help"][..],
            &["-h"],
            &["help"],
            &["play", "--help"],
            &["render", "session.json", "-h"],
            &["record", "--help"],
            &["recover", "-h"],
            &["unknown", "--help"],
        ] {
            assert_eq!(parsed(arguments), Ok(Command::Help), "{arguments:?}");
        }
    }

    #[test]
    fn seconds_are_read_exactly() {
        let read = |text| seconds("--from", text);
        assert_eq!(read("0"), Ok(TimePosition(0)));
        assert_eq!(read("2"), Ok(TimePosition(2 * SECOND)));
        assert_eq!(read("2."), Ok(TimePosition(2 * SECOND)));
        assert_eq!(read(".25"), Ok(TimePosition(SECOND / 4)));
        assert_eq!(read("0.000001"), Ok(TimePosition(SECOND / 1_000_000)));
        assert_eq!(read("007.5"), Ok(TimePosition(15 * SECOND / 2)));
        // A picosecond is 0.903 time units: placed on the next one.
        assert_eq!(read("0.000000000001"), Ok(TimePosition(1)));
        assert_eq!(read("0.000000000000000001"), Ok(TimePosition(1)));
        let most = i64::MAX / SECOND;
        assert_eq!(read(&most.to_string()), Ok(TimePosition(most * SECOND)));
    }

    #[test]
    fn seconds_that_are_not_a_plain_number_are_refused() {
        for text in [
            "", ".", "-1", "+1", "1e3", "1.2.3", " 1", "1 ", "inf", "NaN", "1,5", "½",
        ] {
            let error = seconds("--from", text).unwrap_err().to_string();
            assert!(
                error.contains("--from takes a number of seconds"),
                "{text:?}: {error}"
            );
        }
        assert!(
            seconds("--from", "0.0000000000000000001")
                .unwrap_err()
                .to_string()
                .contains("at most 18 decimals")
        );
        for text in [
            &(i64::MAX / SECOND + 1).to_string()[..],
            "99999999999999999999999999999999999999999",
        ] {
            assert!(
                seconds("--seconds", text)
                    .unwrap_err()
                    .to_string()
                    .contains("at most 10212244 seconds, about 118 days"),
                "{text}"
            );
        }
    }

    #[test]
    fn each_misuse_is_said_plainly() {
        let cases: [(&[&str], &str); 31] = [
            (&[], "no command given"),
            (&["paly", "session.json"], "unknown command \"paly\""),
            (&["play"], "play needs a session file"),
            (&["play", "a.json", "b.json"], "\"b.json\" is one too many"),
            (
                &["play", "a.json", "--loud"],
                "play has no option \"--loud\"",
            ),
            (
                &["play", "a.json", "--block", "64"],
                "play has no option \"--block\"",
            ),
            (&["play", "a.json", "--server"], "--server needs a value"),
            (
                &["play", "a.json", "--server", ""],
                "--server needs a server's name, not an empty one",
            ),
            (
                &["play", "a.json", "--server", "--connect"],
                "--server needs a value, not the option \"--connect\"",
            ),
            (
                &["play", "a.json", "--connect", "--connect"],
                "--connect is given twice",
            ),
            (
                &["play", "a.json", "--from", "1", "--from", "2"],
                "--from is given twice",
            ),
            (
                &["play", "a.json", "--from", "-1"],
                "--from takes a number of seconds",
            ),
            (
                &["play", "a.json", "--seconds", "0"],
                "--seconds takes a number of seconds above 0",
            ),
            (&["play", "a.json", "-"], "play has no option \"-\""),
            (
                &["render", "a.json"],
                "render needs a session file and the WAV file",
            ),
            (
                &["render", "a.json", "b.wav", "c.wav"],
                "\"c.wav\" is one too many",
            ),
            (
                &["render", "a.json", "b.wav", "--server", "x"],
                "render has no option \"--server\"",
            ),
            (
                &["render", "a.json", "b.wav", "--block", "-64"],
                "--block takes a whole number of frames, not \"-64\"",
            ),
            (
                &["render", "a.json", "b.wav", "--block", "6.4"],
                "--block takes a whole number of frames",
            ),
            (
                &["record"],
                "record needs a session file and a recording folder",
            ),
            (
                &["record", "a.json"],
                "record needs a session file and a recording folder",
            ),
            (
                &["record", "a.json", "takes", "more"],
                "record takes a session file and a recording folder; \"more\" is one too many",
            ),
            (
                &["record", "a.json", "takes", "--block", "64"],
                "record has no option \"--block\"",
            ),
            (
                &["record", "a.json", "takes", "--extra-latency", "-1"],
                "--extra-latency takes a whole number of frames, not \"-1\"",
            ),
            (
                &["record", "a.json", "takes", "--extra-latency", "65537"],
                "--extra-latency takes at most 65536 frames, not 65537",
            ),
            (
                &["record", "a.json", "takes", "--format", "wav"],
                "--format takes float32 or int24, not \"wav\"",
            ),
            (
                &[
                    "record", "a.json", "takes", "--format", "int24", "--format", "int24",
                ],
                "--format is given twice",
            ),
            (
                &["record", "a.json", "takes", "--seconds", "0"],
                "--seconds takes a number of seconds above 0",
            ),
            (&["recover"], "recover needs a recording folder"),
            (
                &["recover", "a", "b"],
                "recover takes one recording folder; \"b\" is one too many",
            ),
            (
                &["recover", "a", "--force"],
                "recover has no option \"--force\"",
            ),
        ];
        for (arguments, expected) in cases {
            let error = refused(arguments);
            assert!(error.contains(expected), "{arguments:?}: {error}");
        }
        assert!(
            refused(&[
                "render",
                "a.json",
                "b.wav",
                "--block",
                "99999999999999999999999"
            ])
            .contains("--block takes fewer frames than 99999999999999999999999")
        );
    }

    #[test]
    fn arguments_that_are_not_text_are_paths_or_refused() {
        let not_text = OsString::from_vec(vec![b'-', 0xFF]);
        let error = parse([OsString::from("play"), OsString::from("a.json"), not_text])
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown option"), "{error}");

        let error = parse([
            OsString::from("play"),
            OsString::from("a.json"),
            OsString::from("--server"),
            OsString::from_vec(vec![0xFF]),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("--server takes text"), "{error}");

        let error = parse([OsString::from_vec(vec![0xFF])])
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown command"), "{error}");

        // A path need not be text.
        let path = OsString::from_vec(vec![b's', 0xFF, b'.', b'j']);
        let Ok(Command::Play(options)) = parse([OsString::from("play"), path.clone()]) else {
            panic!("a path that is not text is a path");
        };
        assert_eq!(options.session, PathBuf::from(path));
    }
}
