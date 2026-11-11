//! The path of the field at a line and column of a JSON text, such as `tracks[1].clips[0].gain`,
//! to name the field of an error serde_json reports. serde_json gives the line and column of an
//! error, and names an unknown or missing field, but not the path to it.

/// What the walk is inside: an object, at its last key read, or an array, at an element.
enum Level {
    Object {
        /// The last key read; `None` before the first, and after each comma.
        key: Option<String>,
        /// Whether the next string is a key.
        expects_key: bool,
    },
    Array {
        index: usize,
    },
}

/// The path of the value being read at `line` and `column` of `text`, as serde_json counts them:
/// lines from 1, columns as the bytes of the line read so far. Empty at the top level.
///
/// It walks the text up to there, keeping the keys and indices it is inside. For an error in a
/// value, that is the value's field; for an unknown field, the field itself; for a missing field,
/// the object that lacks it, whose end has just been read. A key's escapes are kept as written.
pub(super) fn field_at(text: &str, line: usize, column: usize) -> String {
    // Line 0, which serde_json gives an error without a position, reads as line 1; a line past
    // the text, as its end.
    let start_of_line = match line.checked_sub(2) {
        None => 0,
        Some(newlines_before) => text
            .match_indices('\n')
            .nth(newlines_before)
            .map_or(text.len(), |(newline, _)| newline.saturating_add(1)),
    };
    let end = start_of_line.saturating_add(column).min(text.len());
    let read = text.as_bytes().get(..end).unwrap_or_default();
    path(&levels(read))
}

/// The levels the walk is inside after reading `bytes`.
fn levels(bytes: &[u8]) -> Vec<Level> {
    let mut levels = Vec::new();
    let mut bytes = bytes.iter();
    while let Some(&byte) = bytes.next() {
        match byte {
            b'{' => levels.push(Level::Object {
                key: None,
                expects_key: true,
            }),
            b'[' => levels.push(Level::Array { index: 0 }),
            b'}' | b']' => {
                levels.pop();
            }
            b',' => match levels.last_mut() {
                Some(Level::Object { key, expects_key }) => {
                    *key = None;
                    *expects_key = true;
                }
                Some(Level::Array { index }) => *index = index.saturating_add(1),
                None => {}
            },
            b':' => {
                if let Some(Level::Object { expects_key, .. }) = levels.last_mut() {
                    *expects_key = false;
                }
            }
            b'"' => {
                let string = read_string(&mut bytes);
                if let Some(Level::Object { key, expects_key }) = levels.last_mut()
                    && *expects_key
                {
                    *key = Some(String::from_utf8_lossy(&string).into_owned());
                }
            }
            _ => {}
        }
    }
    levels
}

/// The bytes of a string up to its closing quote, which it consumes, or to the end; escapes are
/// kept as written.
fn read_string(bytes: &mut std::slice::Iter<'_, u8>) -> Vec<u8> {
    let mut string = Vec::new();
    while let Some(&byte) = bytes.next() {
        match byte {
            b'"' => break,
            b'\\' => {
                string.push(byte);
                if let Some(&escaped) = bytes.next() {
                    string.push(escaped);
                }
            }
            _ => string.push(byte),
        }
    }
    string
}

/// The levels as a path: `.key` for an object's key, `[index]` for an array's element, without a
/// leading dot.
fn path(levels: &[Level]) -> String {
    let mut path = String::new();
    for level in levels {
        match level {
            Level::Object { key: Some(key), .. } => {
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
            }
            Level::Object { key: None, .. } => {}
            Level::Array { index } => path.push_str(&format!("[{index}]")),
        }
    }
    path
}
