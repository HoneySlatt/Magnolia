//! What the fuzz targets of the test session's parser (`session_file`) and loader (`session_open`)
//! share: the text an input stands for, and [`often`].
//!
//! From an empty corpus, as in CI, raw bytes seldom grow into JSON that follows the schema within
//! seconds. So most inputs build a session, through the target's own builder, written as JSON and
//! sometimes cut short or with a byte changed; the rest are read as text as they are.

use std::borrow::Cow;

use libfuzzer_sys::arbitrary::{Arbitrary, Result, Unstructured};
use magnolia_session::file::SessionFile;

/// The text `input` stands for: one input in eight, by its first byte, is the text itself, if it
/// is UTF-8; the others are the session `build` makes of the whole input, written as JSON, which
/// one in eight then has cut short or a byte changed. `None` for raw bytes that are not UTF-8, and
/// for an input that runs out before its session is whole, which is not a failure.
pub fn session_text<'a>(
    input: &'a [u8],
    build: impl FnOnce(&mut Unstructured<'a>) -> Result<SessionFile>,
) -> Option<Cow<'a, str>> {
    match input.split_first() {
        Some((&mode, raw)) if mode < 32 => std::str::from_utf8(raw).ok().map(Cow::Borrowed),
        _ => {
            let mut input = Unstructured::new(input);
            let session = build(&mut input).ok()?;
            let mut text = session
                .to_json()
                .expect("a session built of plain values writes");
            if input.ratio(1, 8).unwrap_or(false) {
                let at = input.int_in_range(0..=text.len()).unwrap_or(0);
                // Cut, or change one byte, where a character starts.
                if text.is_char_boundary(at) {
                    if input.ratio(1, 2).unwrap_or(false) {
                        text.truncate(at);
                    } else if at < text.len() && text.is_char_boundary(at + 1) {
                        let byte: u8 = input.int_in_range(0x20..=0x7E).unwrap_or(b'x');
                        text.replace_range(at..at + 1, &char::from(byte).to_string());
                    }
                }
            }
            Some(Cow::Owned(text))
        }
    }
}

/// What `likely` gives, fifteen times in sixteen; an arbitrary value otherwise.
pub fn often<'a, T: Arbitrary<'a>>(
    input: &mut Unstructured<'a>,
    likely: impl FnOnce(&mut Unstructured<'a>) -> Result<T>,
) -> Result<T> {
    if input.ratio(1, 16)? {
        input.arbitrary()
    } else {
        likely(input)
    }
}
