//! Magnolia's audio engine: the real-time path that plays, records and monitors
//! ([ADR-0007](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0007-the-audio-engine.md)).
//!
//! It holds no code yet. Its first test proves that RealtimeSanitizer, which checks the
//! real-time path in CI, stops a real-time function that allocates
//! (`tests/realtime_sanitizer.rs`).
