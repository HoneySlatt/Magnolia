# 0023. The real-time allowlist

- Status: Accepted; amends 0007 (what the allowlist holds, how it is suppressed); amended by 0026 and 0032
  (the disk reader's wake)
- Date: 2026-11-11

## Context

[ADR-0007](0007-the-audio-engine.md) lets the real-time path make one kind of system call, on an
allowlist: waking workers and plugin hosts (futex, eventfd). RealtimeSanitizer flags every system
call, so the allowed ones are suppressed by name. M1.3 brings two more needs:

- **The collector**, which frees what the real-time thread lets go, sleeps until woken
  ([glossary](../glossary.md)). Waking it is a futex wake, through `std::thread::Thread::unpark`.
- **Saying whether the audio thread runs in real time**, which ADR-0007 requires. With PipeWire and
  rtkit, the kernel alone knows: glibc's cached policy is stale once rtkit has changed it from
  outside, and the server's own answer says nothing of the thread's rights. Another thread can ask
  the kernel, with `sched_getscheduler`, only if it knows the audio thread's id, which only that
  thread can read, with `gettid`.

Measured on the pinned nightly: RealtimeSanitizer intercepts std's futex calls through the C
library's `syscall`, the same function for a wake and for a contended lock's wait. Symbolized, its
report names every function on the stack.

## Decision

**The allowlist holds:**

- futex wakes of Magnolia's own threads: workers, plugin hosts and the collector;
- `gettid`, once, when the audio thread starts, in the backend's thread-init callback.

**Each allowed call is made by a small function of Magnolia's that says so in its name**, such as
`wake_collector`. The sanitizer's suppressions name those functions
(`call-stack-contains:`), never an intercepted function such as `syscall`: a contended lock's wait,
which goes through the same `syscall`, is still reported.

**The sanitizer's runs are symbolized**, with `allow_addr2line=true`, so that the suppressions see
names; binutils' `addr2line` is in the development shell.

## Alternatives considered

- **Suppressing `syscall`**: one line, but it hides every futex wait, the blocking that the
  sanitizer is there to find.
- **Finding the audio thread without `gettid`**, by listing the process's threads and looking for
  one in real time: no system call on the audio thread, but it cannot tell which thread is the
  audio thread, so the report could be wrong.
- **Asking glibc or the server instead of the kernel**: glibc caches a policy that rtkit changes
  behind its back, and the server answers whether it runs in real time, not whether the thread got
  the rights.

## Consequences

- A new allowed call needs a function of its own, its suppression line, and an amendment here.
- A test proves that a contended lock's wait, made beside an allowed wake, is still reported.
