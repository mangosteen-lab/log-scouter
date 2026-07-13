//! The background worker: one thread, one tokio runtime, one job at a time.
//!
//! The synchronous TUI loop cannot `.await`, so the network call lives here. The main
//! thread sends an `AgentRequest` and drains `AgentEvent`s each frame; the worker runs the
//! request on its runtime and sends the outcome back. Every message carries a `generation`
//! so a reply to a question the user has since cancelled can be dropped on arrival.

use crate::ai::config::AiConfig;
use crate::ai::message::{Assistant, ChatMsg, ToolSpec};
use crate::ai::provider;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

/// One completion for the worker to run.
pub struct AgentRequest {
    pub generation: u64,
    pub config: AiConfig,
    pub key: String,
    pub conversation: Vec<ChatMsg>,
    pub tools: Vec<ToolSpec>,
}

/// The worker's reply, tagged with the generation it belongs to.
pub struct AgentEvent {
    pub generation: u64,
    pub result: Result<Assistant, String>,
}

/// Owns the channels to a running worker thread. Dropping it closes the request channel,
/// which ends the thread's loop.
pub struct AiWorker {
    tx: Sender<AgentRequest>,
    rx: Receiver<AgentEvent>,
    _handle: JoinHandle<()>,
}

impl AiWorker {
    /// Spawn the worker. The thread builds a current-thread runtime and a shared HTTP
    /// client, then serves requests until the sender is dropped.
    pub fn spawn() -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<AgentRequest>();
        let (event_tx, event_rx) = std::sync::mpsc::channel::<AgentEvent>();

        let handle = std::thread::Builder::new()
            .name("logscout-ai".to_string())
            .spawn(move || run(request_rx, event_tx))
            .expect("spawn ai worker");

        Self {
            tx: request_tx,
            rx: event_rx,
            _handle: handle,
        }
    }

    /// Queue a request. Fails only if the worker thread has gone away.
    pub fn send(&self, request: AgentRequest) -> Result<(), String> {
        self.tx
            .send(request)
            .map_err(|_| "ai worker stopped".to_string())
    }

    /// Take the next reply if one has arrived, without blocking the frame.
    pub fn poll(&self) -> Option<AgentEvent> {
        self.rx.try_recv().ok()
    }
}

fn run(requests: Receiver<AgentRequest>, events: Sender<AgentEvent>) {
    // A current-thread runtime is enough: this thread runs exactly one request at a time.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            // Report against generation 0 so the panel can surface the failure.
            let _ = events.send(AgentEvent {
                generation: 0,
                result: Err(format!("could not start async runtime: {error}")),
            });
            return;
        }
    };
    let client = reqwest::Client::new();

    while let Ok(request) = requests.recv() {
        let result = runtime.block_on(provider::complete(
            &client,
            &request.config,
            &request.key,
            &request.conversation,
            &request.tools,
        ));
        // A closed event channel means the app is shutting down; stop.
        if events
            .send(AgentEvent {
                generation: request.generation,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}
