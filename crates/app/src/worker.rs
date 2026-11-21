//! Background session worker. `streamlens_core::Session` is blocking and
//! single-threaded; it lives on its own thread and talks to the UI through
//! channels surfaced as an iced subscription.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use iced::futures::channel::mpsc as iced_mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use streamlens_core::{DecodedFrame, Playback, Session, StreamIndex, Sweep, SweepFrame};

/// Commands from the UI to the worker.
#[derive(Debug, Clone)]
pub enum Command {
    Open(PathBuf),
    /// Decode and deliver a frame (decode-order index).
    Frame(u32),
    /// Start streaming frames in display order from this display position.
    /// `fast` ignores the frame-rate pacing (decode-bound playback).
    Play {
        from_display: u32,
        fast: bool,
    },
    Stop,
    /// Enable/disable reconstruction-stage capture (invalidates the frame
    /// cache; `refresh` is re-decoded and re-sent).
    SetStages {
        on: bool,
        refresh: u32,
    },
    /// Recorded syntax trees for a frame's AU (Headers panel).
    Headers(u32),
    /// Set (or clear) the reference YUV path; restarts the sweep.
    SetReference(Option<PathBuf>),
}

/// Events from the worker to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    /// Subscription is live; send commands through this.
    Ready(std_mpsc::Sender<Command>),
    IndexProgress {
        frames: u32,
        fraction: Option<f32>,
    },
    Opened(Arc<StreamIndex>),
    Frame(Arc<DecodedFrame>),
    Headers(u32, Arc<Vec<streamlens_core::RecordedNal>>),
    /// Playback reached the last frame (or failed) and stopped.
    PlayEnded,
    /// A batch of sweep results arrived.
    Sweep {
        done: u32,
        total: u32,
        batch: Vec<(u32, SweepFrame)>,
    },
    Error(String),
}

/// The subscription stream: spawns the worker thread and forwards its events.
pub fn run() -> impl Stream<Item = Event> {
    iced::stream::channel(64, async |mut output: iced_mpsc::Sender<Event>| {
        let (cmd_tx, cmd_rx) = std_mpsc::channel::<Command>();
        let (evt_tx, mut evt_rx) = iced_mpsc::unbounded::<Event>();

        std::thread::Builder::new()
            .name("session-worker".into())
            .spawn(move || Worker::new(evt_tx).run(cmd_rx))
            .expect("spawn session worker");

        let _ = output.send(Event::Ready(cmd_tx)).await;
        while let Some(e) = evt_rx.next().await {
            if output.send(e).await.is_err() {
                return;
            }
        }
    })
}

/// How far past the last served frame the worker decodes ahead while idle.
/// Covers the decoder's reorder delay plus the next I-frame, so stepping
/// never waits for them on the critical path.
const PREFETCH: u32 = 16;

struct Worker {
    evt_tx: iced_mpsc::UnboundedSender<Event>,
    session: Option<Session>,
    play: Option<Playback>,
    /// Decode-ahead window: (next index to warm, last index to warm).
    prefetch: Option<(u32, u32)>,
    /// Active sweep; dropping it cancels the thread.
    sweep: Option<Sweep>,
    /// Inputs needed to (re)start the sweep.
    path: Option<PathBuf>,
    ref_path: Option<PathBuf>,
}

impl Worker {
    fn new(evt_tx: iced_mpsc::UnboundedSender<Event>) -> Self {
        Self {
            evt_tx,
            session: None,
            play: None,
            prefetch: None,
            sweep: None,
            path: None,
            ref_path: None,
        }
    }

    fn run(mut self, cmd_rx: std_mpsc::Receiver<Command>) {
        loop {
            let mut cmd = match cmd_rx.try_recv() {
                Ok(c) => c,
                Err(std_mpsc::TryRecvError::Empty) => {
                    if self.tick() {
                        continue; // re-check the queue between work items
                    }
                    match cmd_rx.recv() {
                        Ok(c) => c,
                        Err(_) => return,
                    }
                }
                Err(std_mpsc::TryRecvError::Disconnected) => return,
            };

            // Coalesce bursts of frame requests (holding an arrow key can
            // queue them faster than 4K frames decode): only the newest
            // matters, but commands of other kinds keep their order.
            if matches!(cmd, Command::Frame(_)) {
                while let Ok(next) = cmd_rx.try_recv() {
                    if matches!(next, Command::Frame(_)) {
                        cmd = next;
                    } else {
                        self.handle(cmd);
                        cmd = next;
                        break;
                    }
                }
            }
            self.handle(cmd);
        }
    }

    /// One unit of background work. Returns false when idle (caller blocks
    /// on the command queue).
    fn tick(&mut self) -> bool {
        // Playback: emit the next due frame.
        if let (Some(s), Some(p)) = (self.session.as_mut(), self.play.as_mut()) {
            let event = match p.step(s) {
                Ok(Some(frame)) => Event::Frame(frame),
                Ok(None) | Err(_) => {
                    self.play = None;
                    self.set_sweep_paused(false);
                    Event::PlayEnded
                }
            };
            self.send(event);
            return true;
        }
        // Idle: warm the cache one frame at a time.
        if let (Some(s), Some((next, until))) = (self.session.as_mut(), self.prefetch) {
            if next <= until && (next as usize) < s.index.frames.len() {
                self.prefetch = s.frame(next).is_ok().then_some((next + 1, until));
                return true;
            }
            self.prefetch = None;
        }
        false
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Open(path) => self.open(path),
            Command::Frame(idx) => {
                self.play = None;
                self.set_sweep_paused(false);
                self.prefetch = Some((idx + 1, idx + PREFETCH));
                self.deliver(idx);
            }
            Command::Play { from_display, fast } => {
                if let Some(s) = self.session.as_mut() {
                    // The sweep yields its decode capacity while playing.
                    if let Some(sweep) = &self.sweep {
                        sweep.set_paused(true);
                    }
                    let mut p = Playback::new(s, from_display, fast);
                    // Warm the first stretch so playback starts smooth.
                    p.prebuffer(s);
                    self.play = Some(p);
                    self.prefetch = None;
                }
            }
            Command::Stop => {
                self.play = None;
                self.set_sweep_paused(false);
            }
            Command::SetStages { on, refresh } => {
                if let Some(s) = self.session.as_mut() {
                    s.set_stages(on);
                    self.deliver(refresh);
                }
            }
            Command::Headers(idx) => {
                let Some(s) = self.session.as_ref() else {
                    return;
                };
                let event = match s.headers(idx) {
                    Ok(h) => Event::Headers(idx, Arc::new(h)),
                    Err(e) => Event::Error(e.to_string()),
                };
                self.send(event);
            }
            Command::SetReference(ref_path) => {
                self.ref_path = ref_path;
                self.restart_sweep();
            }
        }
    }

    fn open(&mut self, path: PathBuf) {
        self.play = None;
        self.prefetch = None;
        self.sweep = None; // cancel; a new one starts after Opened
        self.session = None; // drop the previous stream before indexing
        self.path = Some(path.clone());

        let tx = self.evt_tx.clone();
        let progress = move |p: streamlens_core::IndexProgress| {
            let _ = tx.unbounded_send(Event::IndexProgress {
                frames: p.frames_indexed,
                fraction: p.fraction,
            });
        };
        match Session::open(&path, progress) {
            Ok(s) => {
                self.send(Event::Opened(s.index.clone()));
                self.session = Some(s);
                self.restart_sweep();
            }
            Err(e) => self.send(Event::Error(e.to_string())),
        }
    }

    fn restart_sweep(&mut self) {
        self.sweep = None;
        let (Some(path), Some(s)) = (self.path.clone(), self.session.as_ref()) else {
            return;
        };
        let total = s.index.frames.len() as u32;
        let tx = self.evt_tx.clone();
        self.sweep = Some(Sweep::spawn(
            path,
            self.ref_path.clone(),
            move |done, batch| {
                let _ = tx.unbounded_send(Event::Sweep { done, total, batch });
            },
        ));
    }

    fn set_sweep_paused(&self, paused: bool) {
        if let Some(sweep) = &self.sweep {
            sweep.set_paused(paused);
        }
    }

    /// Decodes and sends one frame (or the error).
    fn deliver(&mut self, idx: u32) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        let event = match s.frame(idx) {
            Ok(f) => Event::Frame(f),
            Err(e) => Event::Error(format!("decode {idx}: {e}")),
        };
        self.send(event);
    }

    fn send(&self, event: Event) {
        let _ = self.evt_tx.unbounded_send(event);
    }
}
