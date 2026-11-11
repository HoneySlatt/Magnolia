//! The backend's handle, [`JackBackend`]: it opens a JACK client around the engine's processor, and
//! stays on the caller's thread while the audio thread plays.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use jack::{AsyncClient, AudioIn, AudioOut, Client, ClientOptions, ClientStatus, PortFlags};
use magnolia_engine::engine::Processor;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::callback::{Callback, CallbackParts};
use crate::notifications::{Notifications, Shared};
use crate::realtime::{self, AudioServer, RealtimeStatus, ThreadScheduling};

/// What [`JackBackend::open`] opens.
pub struct JackOptions {
    /// The client's name, as the server and its patchbays show it. The server may add a suffix if
    /// another client holds it: [`JackBackend::client_name`] gives the name it got.
    pub client_name: String,
    /// The server to connect to, by name; `None` for the default one.
    pub server_name: Option<String>,
    /// The engine's processor, which the audio thread will call once a period. The server must
    /// run at its rate; the client gets one input port per capture channel (`capture_1` and on)
    /// and one output port per playback channel (`playback_1` and on).
    pub processor: Processor,
}

/// Plays an engine's [`Processor`] through the JACK API, on PipeWire or on JACK2.
///
/// [`open`](Self::open) opens a client with one port per engine channel and activates it. From
/// then on, the server calls the processor once a period on its audio thread, until
/// [`close`](Self::close) or the drop of the backend, both on the caller's thread. The backend
/// never starts a server.
///
/// Around each period it counts, in the processor's
/// [`EngineStats`](magnolia_engine::stats::EngineStats): its own overruns of the period's
/// deadline, gaps in the server's frame time, and the xruns the server reports. It records the
/// audio thread's id there too, which [`realtime_status`](Self::realtime_status) reads to say
/// whether that thread runs in real time.
///
/// A poisoned processor is rebuilt in place: [`replace_processor`](Self::replace_processor) hands a
/// new one to the audio thread, which swaps it in at a period's start; the client stays active and
/// its connections stay. The old one comes back to the handle, which drops it.
pub struct JackBackend {
    /// `None` only while closing.
    client: Option<AsyncClient<Notifications, Callback>>,
    replacements: Producer<Box<Processor>>,
    replaced: Consumer<Box<Processor>>,
    shared: Arc<Shared>,
    client_name: String,
    capture_ports: Vec<String>,
    playback_ports: Vec<String>,
    engine_rate: u32,
    server: AudioServer,
    library: Option<PathBuf>,
}

impl JackBackend {
    /// Opens a client on a running server, registers its ports and activates it: the processor
    /// plays from the next period. It never starts a server.
    pub fn open(options: JackOptions) -> Result<Self, OpenError> {
        let JackOptions {
            client_name,
            server_name,
            processor,
        } = options;
        let capture_channels = processor.capture_channels();
        let playback_channels = processor.playback_channels();
        let opened = match &server_name {
            Some(server) => {
                Client::new_with_server_name(&client_name, server, ClientOptions::NO_START_SERVER)
            }
            None => Client::new(&client_name, ClientOptions::NO_START_SERVER),
        };
        let (client, _) =
            opened.map_err(|error| OpenError::from_client_error(error, server_name))?;

        let engine_rate = processor.sample_rate().hertz();
        let server_rate = client.sample_rate();
        if server_rate != engine_rate {
            return Err(OpenError::RateDiffers {
                server: server_rate,
                engine: engine_rate,
            });
        }

        let capture = (1..=capture_channels)
            .map(|channel| client.register_port(&format!("capture_{channel}"), AudioIn::default()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(OpenError::refused("register a capture port"))?;
        let playback = (1..=playback_channels)
            .map(|channel| {
                client.register_port(&format!("playback_{channel}"), AudioOut::default())
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(OpenError::refused("register a playback port"))?;
        let capture_ports = capture
            .iter()
            .map(|port| port.name())
            .collect::<Result<Vec<_>, _>>()
            .map_err(OpenError::refused("name a capture port"))?;
        let playback_ports = playback
            .iter()
            .map(|port| port.name())
            .collect::<Result<Vec<_>, _>>()
            .map_err(OpenError::refused("name a playback port"))?;
        let client_name = client.name().to_owned();
        let library = realtime::loaded_libjack();
        let server = AudioServer::of_library(library.as_deref());

        let shared = Arc::new(Shared::new(server_rate));
        // One replacement in flight at most, and room to send back the one it replaces.
        let (replacements, replacements_in_callback) = RingBuffer::new(1);
        let (replaced_by_callback, replaced) = RingBuffer::new(1);
        let callback = Callback::new(CallbackParts {
            processor: Box::new(processor),
            capture,
            playback,
            replacements: replacements_in_callback,
            replaced: replaced_by_callback,
            shared: Arc::clone(&shared),
            engine_rate,
        });
        let client = client
            .activate_async(Notifications::new(Arc::clone(&shared)), callback)
            .map_err(OpenError::refused("activate the client"))?;
        Ok(Self {
            client: Some(client),
            replacements,
            replaced,
            shared,
            client_name,
            capture_ports,
            playback_ports,
            engine_rate,
            server,
            library,
        })
    }

    /// The client's name, as the server gave it.
    pub fn client_name(&self) -> &str {
        &self.client_name
    }

    /// The full names of the client's capture ports (`<client>:capture_1` and on), one per engine
    /// capture channel, in order.
    pub fn capture_ports(&self) -> &[String] {
        &self.capture_ports
    }

    /// The full names of the client's playback ports (`<client>:playback_1` and on), one per
    /// engine playback channel, in order.
    pub fn playback_ports(&self) -> &[String] {
        &self.playback_ports
    }

    /// The server's period, in frames, as it stands now; 0 once closing.
    pub fn period_frames(&self) -> u32 {
        self.client().map_or(0, Client::buffer_size)
    }

    /// The rate the server last reported.
    pub fn server_sample_rate(&self) -> u32 {
        self.shared.server_rate()
    }

    /// Whether the backend's own callback code panicked, outside the processor, which catches its
    /// own panics: the callback then writes silence every period, the client still active and
    /// connected (ADR-0022). A new processor does not help; close and open the backend again.
    pub fn callback_panicked(&self) -> bool {
        self.shared.callback_panicked()
    }

    /// Whether the server shut the client down, as when it stopped or died: the processor is no
    /// longer called. Close the backend.
    pub fn server_shut_down(&self) -> bool {
        self.shared.shut_down()
    }

    /// Whether the audio thread runs in real time, asked of the kernel, with the server in use and
    /// how it grants the rights. Callable from any thread but the audio one; two system calls that
    /// never block. Before the first period, the thread is not known yet. rtkit grants PipeWire's
    /// threads their rights within about a second of the start: ask again then.
    pub fn realtime_status(&self) -> RealtimeStatus {
        let server_runs_in_real_time = match self.server {
            AudioServer::Jack2 if !self.server_shut_down() => self.client().map(|client| {
                // SAFETY: the client is open and active, held by `self`; libjack is loaded since
                // the client opened. `jack_is_realtime` only reads the server's settings.
                unsafe { jack::jack_sys::jack_is_realtime(client.raw()) != 0 }
            }),
            _ => None,
        };
        RealtimeStatus {
            thread: ThreadScheduling::of_thread(self.shared.audio_thread_id()),
            server: self.server,
            library: self.library.clone(),
            server_runs_in_real_time,
        }
    }

    /// Connects each playback port to a physical playback port of the server, in order, as many
    /// as both have, and returns how many it connected. A connection that already exists counts.
    pub fn connect_to_physical_playback(&self) -> Result<usize, ConnectError> {
        let Some(client) = self.client() else {
            return Ok(0);
        };
        let physical = client.ports(
            None,
            Some(jack::jack_sys::FLOAT_MONO_AUDIO),
            PortFlags::IS_INPUT | PortFlags::IS_PHYSICAL,
        );
        let mut connected = 0;
        for (source, destination) in self.playback_ports.iter().zip(&physical) {
            match client.connect_ports_by_name(source, destination) {
                Ok(()) | Err(jack::Error::PortAlreadyConnected(_, _)) => connected += 1,
                Err(error) => {
                    return Err(ConnectError {
                        source: source.clone(),
                        destination: destination.clone(),
                        detail: error.to_string(),
                    });
                }
            }
        }
        Ok(connected)
    }

    /// Hands `processor` to the audio thread, which swaps it in at the start of its next period
    /// and plays it from there: the client stays active and its connections stay. The processor it
    /// replaces comes back to this handle, which drops it here, on a later call or at
    /// [`poll`](Self::poll) or [`close`](Self::close), never on the audio thread.
    ///
    /// Use it to rebuild a poisoned engine
    /// ([ADR-0022](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0022-a-panic-on-the-real-time-path-gives-silence.md)):
    /// make a new engine at the same rate, hand its processor here, and commit the graph again on
    /// its controller. Gives the processor back, unused, when its rate or channels differ from
    /// the one the client was opened for, or while a replacement handed earlier still waits for
    /// the audio thread.
    pub fn replace_processor(&mut self, processor: Processor) -> Result<(), ReplaceError> {
        self.poll();
        let matches = processor.sample_rate().hertz() == self.engine_rate
            && processor.capture_channels() == self.capture_ports.len()
            && processor.playback_channels() == self.playback_ports.len();
        if !matches {
            return Err(ReplaceError::Differs(Box::new(processor)));
        }
        self.replacements
            .push(Box::new(processor))
            .map_err(|rtrb::PushError::Full(processor)| ReplaceError::Busy(processor))
    }

    /// Drops, on the caller's thread, the processors the audio thread has replaced. Never blocks.
    pub fn poll(&mut self) {
        while let Ok(old) = self.replaced.pop() {
            drop(old);
        }
    }

    /// Deactivates the client and closes it, and returns the processor the audio thread played
    /// last. Its connections go with it.
    pub fn close(mut self) -> Result<Processor, CloseError> {
        self.deactivate()
    }

    /// Deactivates the client, drops what the audio thread let go, and returns its processor.
    fn deactivate(&mut self) -> Result<Processor, CloseError> {
        let Some(client) = self.client.take() else {
            return Err(CloseError::NotDeactivated);
        };
        let deactivated = client.deactivate();
        self.poll();
        match deactivated {
            Ok((_client, _notifications, callback)) => Ok(callback.into_processor()),
            Err(jack::Error::ClientPanicked) => Err(CloseError::CallbackPanicked),
            Err(_) => Err(CloseError::NotDeactivated),
        }
    }

    fn client(&self) -> Option<&Client> {
        self.client.as_ref().map(AsyncClient::as_client)
    }
}

/// Closes the backend if [`close`](JackBackend::close) did not, on the dropping thread.
impl Drop for JackBackend {
    fn drop(&mut self) {
        if self.client.is_some() {
            // Dropping it drops the processor here; nothing is left to report to.
            let _ = self.deactivate();
        }
    }
}

/// Why [`JackBackend::open`] failed, and what to do about it.
#[derive(Debug)]
pub enum OpenError {
    /// libjack, the JACK API's library, could not be loaded.
    LibraryNotFound(String),
    /// No server runs, or none under the name asked for.
    NoServer {
        /// The server asked for; `None` for the default one.
        server_name: Option<String>,
    },
    /// The server's rate differs from the engine's.
    RateDiffers {
        /// The server's rate, in hertz.
        server: u32,
        /// The engine's rate, in hertz.
        engine: u32,
    },
    /// The server refused a step of the opening.
    Refused {
        /// The step, such as "activate the client".
        step: &'static str,
        /// What the JACK library said.
        detail: String,
    },
}

impl OpenError {
    /// The error of a failed `jack_client_open`.
    fn from_client_error(error: jack::Error, server_name: Option<String>) -> Self {
        match error {
            jack::Error::LibraryError(detail) => Self::LibraryNotFound(detail),
            jack::Error::ClientError(status) if status.contains(ClientStatus::SERVER_FAILED) => {
                Self::NoServer { server_name }
            }
            error => Self::Refused {
                step: "open a client",
                detail: error.to_string(),
            },
        }
    }

    /// Makes the error of a step the server refused.
    fn refused(step: &'static str) -> impl Fn(jack::Error) -> Self {
        move |error| Self::Refused {
            step,
            detail: error.to_string(),
        }
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryNotFound(detail) => write!(
                formatter,
                "libjack, the library of the JACK API, was not found ({detail}). Install \
                 PipeWire's JACK support (often a package named pipewire-jack) or JACK2, or run \
                 Magnolia through `pw-jack`."
            ),
            Self::NoServer { server_name } => {
                let named = server_name
                    .as_ref()
                    .map(|name| format!(" under the name {name:?}"))
                    .unwrap_or_default();
                write!(
                    formatter,
                    "no JACK server is running{named}. Start PipeWire with its JACK support, or \
                     JACK2's `jackd`, then try again: Magnolia never starts a server itself."
                )
            }
            Self::RateDiffers { server, engine } => write!(
                formatter,
                "the JACK server runs at {server} Hz and the engine at {engine} Hz. Set the \
                 server's rate to {engine} Hz, or make the engine at {server} Hz."
            ),
            Self::Refused { step, detail } => {
                write!(formatter, "the JACK server refused to {step}: {detail}")
            }
        }
    }
}

impl Error for OpenError {}

/// Why [`JackBackend::close`] could not return the processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseError {
    /// The server did not deactivate the client, as when it has died. Since the server may still
    /// point into the callback's state, the `jack` crate keeps that state, the processor with it,
    /// for the rest of the process's life: its memory and its collector thread are not freed.
    NotDeactivated,
    /// The callback panicked where even its own catch could not: while writing silence after a
    /// first panic. The `jack` crate then stopped calling it and dropped the processor.
    CallbackPanicked,
}

/// Why [`JackBackend::replace_processor`] gave a processor back, unused.
pub enum ReplaceError {
    /// Its rate or its channels differ from those the client was opened for: open a new backend
    /// for it instead.
    Differs(Box<Processor>),
    /// A replacement handed earlier still waits for the audio thread: try again after a period.
    Busy(Box<Processor>),
}

impl ReplaceError {
    /// The processor given back.
    pub fn into_processor(self) -> Processor {
        match self {
            Self::Differs(processor) | Self::Busy(processor) => *processor,
        }
    }
}

impl fmt::Debug for ReplaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Differs(_) => "ReplaceError::Differs(..)",
            Self::Busy(_) => "ReplaceError::Busy(..)",
        })
    }
}

impl fmt::Display for ReplaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Differs(_) => {
                "the new processor's rate or channels differ from those the JACK client was \
                 opened for; open a new backend for it"
            }
            Self::Busy(_) => {
                "a processor handed earlier still waits for the audio thread; try again after a \
                 period"
            }
        })
    }
}

impl std::error::Error for ReplaceError {}

impl fmt::Display for CloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotDeactivated => {
                "the JACK server did not deactivate the client, as when it has died; the \
                 processor stays allocated until the process ends"
            }
            Self::CallbackPanicked => "the JACK backend's process callback panicked",
        })
    }
}

impl Error for CloseError {}

/// Why [`JackBackend::connect_to_physical_playback`] failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectError {
    /// The port to connect from.
    pub source: String,
    /// The port to connect to.
    pub destination: String,
    /// What the JACK library said.
    pub detail: String,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the JACK server did not connect {} to {}: {}",
            self.source, self.destination, self.detail
        )
    }
}

impl Error for ConnectError {}

#[cfg(test)]
mod tests {
    use jack::ClientStatus;

    use super::OpenError;

    #[test]
    fn each_open_error_says_what_to_do() {
        let missing = OpenError::from_client_error(
            jack::Error::LibraryError("libjack.so.0: cannot open shared object file".to_owned()),
            None,
        );
        assert!(matches!(missing, OpenError::LibraryNotFound(_)));
        let text = missing.to_string();
        assert!(
            text.contains("pipewire-jack") && text.contains("JACK2") && text.contains("pw-jack")
        );

        let status = ClientStatus::FAILURE | ClientStatus::SERVER_FAILED;
        let no_server =
            OpenError::from_client_error(jack::Error::ClientError(status), Some("x".to_owned()));
        assert!(matches!(no_server, OpenError::NoServer { .. }));
        assert!(no_server.to_string().contains("never starts a server"));

        let rate = OpenError::RateDiffers {
            server: 44_100,
            engine: 48_000,
        };
        assert!(
            rate.to_string()
                .contains("44100 Hz and the engine at 48000 Hz")
        );

        let refused =
            OpenError::from_client_error(jack::Error::ClientError(ClientStatus::FAILURE), None);
        assert!(matches!(refused, OpenError::Refused { .. }));
    }
}
