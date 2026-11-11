//! Memory written before the real-time thread sees it.
//!
//! The kernel maps a page of fresh memory when it is first written. If the first writer were the
//! real-time thread, each new page would cost it a page fault in the middle of a period, which
//! RealtimeSanitizer cannot see. So every buffer, chunk and reserved vector that the controller
//! hands the processor is written here first, on the controller's thread. Writing zeros is not
//! enough: the compiler may turn an allocation followed by a zero fill into `calloc`, which
//! returns fresh pages unwritten. The values go through `black_box`, which the compiler cannot see
//! through, and so do the written buffers.

use std::hint::black_box;
use std::mem::MaybeUninit;

/// `length` copies of `value`, every page of them written.
pub(crate) fn written_slice<T: Copy>(length: usize, value: T) -> Box<[T]> {
    let mut slice = vec![value; length].into_boxed_slice();
    slice.fill(black_box(value));
    black_box(&mut slice);
    slice
}

/// An empty vector with room for `capacity` items, every page of that room written, so that
/// pushing into it on the real-time thread neither allocates nor faults.
pub(crate) fn written_vec<T>(capacity: usize) -> Vec<T> {
    let mut vector = Vec::with_capacity(capacity);
    for slot in vector.spare_capacity_mut() {
        *slot = black_box(MaybeUninit::zeroed());
    }
    black_box(&mut vector);
    vector
}
