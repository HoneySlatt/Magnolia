//! Magnolia's monitoring path adds nothing to a direct connection (ADR-0031): a source client sends
//! an impulse to a sink client by two paths at once, directly, and through Magnolia, from its
//! capture port through the engine to its playback port. Through capture straight to playback, and
//! through a `TestLatency` of 0, the impulse arrives on the same sample by both paths; through a
//! `TestLatency` of `L`, exactly `L` samples later through Magnolia.
//!
//! What it proves: within a JACK cycle, Magnolia's monitoring path delays a signal by the latency
//! of its nodes and nothing more; JACK runs Magnolia between the source and the sink, in the same
//! cycle. What it does not prove: the hardware's round trip, from the output's converter through a
//! cable to the input's, which the dummy driver does not have; the owner measures it with a
//! loopback cable and `jack_iodelay`. Nor are the latencies Magnolia reports checked here
//! (`latency.rs`). It needs JACK's processing path, not its timing: the server freewheels.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use jack::{
    AsyncClient, AudioIn, AudioOut, Client, ClientOptions, Control, Port, ProcessHandler,
    ProcessScope,
};
use magnolia_engine::engine::{Destination, Source};
use magnolia_engine::nodes::TestLatency;

use crate::common::{Server, client_name, pause, step};
use crate::support::{Freewheel, Rig, one_at_a_time};

/// The latency of the latent node, in samples: more than a period of 64 frames, so that the
/// impulse comes out in a later cycle than it went in.
const LATENCY: u32 = 100;

/// Where in its cycle the source writes the impulse.
const OFFSET: u32 = 5;

/// Impulses sent through each of Magnolia's graphs.
const IMPULSES: usize = 3;

/// How long a test waits for an impulse.
const TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn an_impulse_through_magnolia_arrives_with_a_direct_one_or_exactly_its_nodes_latency_later() {
    let _turn = one_at_a_time();
    let server = Server::start(64);
    let mut rig = Rig::open(&server, 1, 1);
    rig.controller
        .connect(Source::Capture(0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.wait_until_steady();
    let impulse = Arc::new(Impulse::default());
    let source = ImpulseSource::open(&server, Arc::clone(&impulse));
    let sink = ImpulseSink::open(&server, Arc::clone(&impulse));
    let patchbay = source.client.as_client();
    for (from, to) in [
        (source.output.as_str(), sink.direct.as_str()),
        (
            source.output.as_str(),
            rig.backend.capture_ports()[0].as_str(),
        ),
        (
            rig.backend.playback_ports()[0].as_str(),
            sink.through.as_str(),
        ),
    ] {
        patchbay
            .connect_ports_by_name(from, to)
            .unwrap_or_else(|error| panic!("{from} -> {to}: {error}"));
    }
    let freewheel = Freewheel::start(&server);

    let straight = (0..IMPULSES).map(|_| impulse.send()).collect::<Vec<_>>();
    // Capture through a node to playback, the node's latency changed at each commit.
    let mut node = None;
    let mut through_node = |rig: &mut Rig, latency: u32| {
        if let Some(old) = node.take() {
            rig.controller.remove_node(old).unwrap();
        } else {
            rig.controller
                .disconnect(Source::Capture(0), Destination::Playback(0))
                .unwrap();
        }
        let latent = rig.add(TestLatency::new(1, latency));
        rig.controller
            .connect(Source::Capture(0), Destination::Node(latent, 0));
        rig.controller
            .connect(Source::Node(latent, 0), Destination::Playback(0));
        rig.controller.commit().unwrap();
        rig.wait_until_steady();
        node = Some(latent);
        (0..IMPULSES).map(|_| impulse.send()).collect::<Vec<_>>()
    };
    let through_zero = through_node(&mut rig, 0);
    let through_latency = through_node(&mut rig, LATENCY);
    freewheel.stop();
    step("closing the sink and the source");
    drop(sink);
    drop(source);

    eprintln!(
        "delays (direct, through Magnolia), in samples: straight {straight:?}, through a latency \
         of 0 {through_zero:?}, of {LATENCY} {through_latency:?}"
    );
    for (what, delays, added) in [
        ("capture straight to playback", &straight, 0),
        ("through a latency of 0", &through_zero, 0),
        ("through a latency of 100", &through_latency, LATENCY),
    ] {
        for &(direct, through) in delays {
            assert_eq!(direct, 0, "{what}: the direct path, within the cycle");
            assert_eq!(through, direct + added, "{what}");
        }
    }
    assert_eq!(rig.controller.stats().non_finite_samples(), 0);
}

/// Where an impulse is: not yet sent, sent and awaited on both of the sink's inputs, or arrived.
const IDLE: u32 = 0;
const ARMED: u32 = 1;
const SENT: u32 = 2;

/// What the source and the sink share: the impulse's state, the frame time at which the source
/// wrote it, and the frame time at which each of the sink's inputs saw it.
#[derive(Default)]
struct Impulse {
    state: AtomicU32,
    sent_at: AtomicU32,
    /// Bit 0 for the direct input, bit 1 for the input through Magnolia.
    arrived: AtomicU32,
    direct_at: AtomicU32,
    through_at: AtomicU32,
}

impl Impulse {
    /// Sends one impulse and waits until both paths have brought it: returns each path's delay,
    /// in samples, by the server's frame time.
    fn send(&self) -> (u32, u32) {
        self.arrived.store(0, Ordering::Release);
        self.state.store(ARMED, Ordering::Release);
        let start = Instant::now();
        while self.arrived.load(Ordering::Acquire) != 0b11 {
            assert!(start.elapsed() < TIMEOUT, "the impulse did not arrive");
            pause(Duration::from_millis(1));
        }
        self.state.store(IDLE, Ordering::Release);
        let sent_at = self.sent_at.load(Ordering::Acquire);
        (
            self.direct_at.load(Ordering::Acquire).wrapping_sub(sent_at),
            self.through_at
                .load(Ordering::Acquire)
                .wrapping_sub(sent_at),
        )
    }
}

/// A plain JACK client that writes one impulse at `OFFSET` in its next cycle once armed, and
/// silence otherwise.
struct ImpulseSource {
    client: AsyncClient<(), SourceHandler>,
    output: String,
}

struct SourceHandler {
    output: Port<AudioOut>,
    impulse: Arc<Impulse>,
}

impl ProcessHandler for SourceHandler {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        let output = self.output.as_mut_slice(scope);
        output.fill(0.0);
        if self.impulse.state.load(Ordering::Acquire) == ARMED {
            output[OFFSET as usize] = 1.0;
            self.impulse.sent_at.store(
                scope.last_frame_time().wrapping_add(OFFSET),
                Ordering::Release,
            );
            self.impulse.state.store(SENT, Ordering::Release);
        }
        Control::Continue
    }
}

impl ImpulseSource {
    fn open(server: &Server, impulse: Arc<Impulse>) -> Self {
        step("opening the source");
        let (client, _) = Client::new_with_server_name(
            &client_name("source"),
            &server.name,
            ClientOptions::NO_START_SERVER,
        )
        .expect("the source opens on the test's server");
        let output = client.register_port("out", AudioOut::default()).unwrap();
        let name = output.name().unwrap();
        let client = client
            .activate_async((), SourceHandler { output, impulse })
            .unwrap();
        Self {
            client,
            output: name,
        }
    }
}

/// A plain JACK client that notes, on each of its two inputs, the frame time of the first sample
/// above one half after an impulse was sent.
struct ImpulseSink {
    _client: AsyncClient<(), SinkHandler>,
    direct: String,
    through: String,
}

struct SinkHandler {
    direct: Port<AudioIn>,
    through: Port<AudioIn>,
    impulse: Arc<Impulse>,
}

impl SinkHandler {
    /// Notes the impulse on `input`, bit `bit` of the arrivals, at its frame time.
    fn look(&self, input: &[f32], scope: &ProcessScope, bit: u32, at: &AtomicU32) {
        if self.impulse.arrived.load(Ordering::Acquire) & bit != 0 {
            return;
        }
        if let Some(index) = input.iter().position(|&sample| sample > 0.5) {
            at.store(
                scope.last_frame_time().wrapping_add(index as u32),
                Ordering::Release,
            );
            self.impulse.arrived.fetch_or(bit, Ordering::AcqRel);
        }
    }
}

impl ProcessHandler for SinkHandler {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        if self.impulse.state.load(Ordering::Acquire) == SENT {
            self.look(
                self.direct.as_slice(scope),
                scope,
                0b01,
                &self.impulse.direct_at,
            );
            self.look(
                self.through.as_slice(scope),
                scope,
                0b10,
                &self.impulse.through_at,
            );
        }
        Control::Continue
    }
}

impl ImpulseSink {
    fn open(server: &Server, impulse: Arc<Impulse>) -> Self {
        step("opening the sink");
        let (client, _) = Client::new_with_server_name(
            &client_name("sink"),
            &server.name,
            ClientOptions::NO_START_SERVER,
        )
        .expect("the sink opens on the test's server");
        let direct = client.register_port("direct", AudioIn::default()).unwrap();
        let through = client.register_port("through", AudioIn::default()).unwrap();
        let (direct_name, through_name) = (direct.name().unwrap(), through.name().unwrap());
        let client = client
            .activate_async(
                (),
                SinkHandler {
                    direct,
                    through,
                    impulse,
                },
            )
            .unwrap();
        Self {
            _client: client,
            direct: direct_name,
            through: through_name,
        }
    }
}
