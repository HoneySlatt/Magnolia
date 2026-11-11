# 0021. The node contract

- Status: Accepted; amended by 0025 and 0027
- Date: 2026-11-11

## Context

The engine runs a graph of nodes ([ADR-0007](0007-the-audio-engine.md)). Every node implements
one trait: today a sine and a gain; from M1.6 the proxy of every plugin host. Changing that trait
later means changing every node, so what the plan already requires goes into it now:

- **Rest.** At rest, plugin hosts receive no calls ([glossary](../glossary.md)). If every node runs
  every block, rest costs the whole graph.
- **A dead or late plugin.** Its block is silent, never bypassed
  ([ADR-0003](0003-every-plugin-out-of-the-engines-process.md)): a bypassed limiter could send a
  loud signal to the headphones.
- **CLAP**, which M1.6 hosts, has a status for each call (`CLAP_PROCESS_SLEEP`, an error whose
  output is discarded) and a mask of constant channels.
- **The null of M1.4**: playback equals an offline render to the last bit, whatever the period.
- **Denormals.** C++ engines set the processor's flush-to-zero mode around their callback, as
  Ardour does. Rust's documentation says that changing that mode is immediate undefined behaviour.

## Decision

The owner's choice of 2026-11-11.

**`process` returns a status.**

- *Continue*: the outputs are valid.
- *Sleep*: the outputs are valid for this block, and the node needs no call while its inputs stay
  silent and no event comes for it. The engine then skips it and writes silence in its place.
- *Error*: the outputs are discarded. The engine writes silence in their place, counts the error and
  calls the node again next block.

**Every channel carries a silence flag.** An input channel is flagged silent when nothing it sums
can sound; it then holds zeros. A node may flag an output silent only if it wrote zeros there.

**A node's channel counts are fixed for its life.** A different layout is a new node, swapped in.

**A node's output does not depend on how blocks are cut.** The engine may split a period into
sub-blocks, at a swap's fade or past its largest block, and a render must equal playback to the
bit.

**Nodes keep denormals out of their own state**, for example by flushing a filter's feedback below
a threshold to zero. Every thread that runs nodes keeps the default floating-point mode.

**Events reach a node with their offset in the block**, sorted by offset, then by arrival.

## Alternatives considered

- **`process` alone, extended at M1.6**: less code now, but every node would be rewritten when
  plugins arrive, and rest would cost the whole graph until then.
- **Flush-to-zero around the callback**: what C++ engines do, but undefined behaviour in Rust, and
  it would make a render depend on the thread that ran it.
- **Silence as a bypass of the failed node**: keeps the music going, but against ADR-0003: a
  bypassed limiter or a bypassed gain at -60 dB can be dangerous.

## Consequences

- A sleeping node costs nothing but a check per block; an idle track's chain sleeps once its
  instrument does.
- Each node's author handles denormals; the engine cannot do it for them.
- The engine owns the rule that a silent channel holds zeros, so a node that ignores the flags
  still reads correct input.
