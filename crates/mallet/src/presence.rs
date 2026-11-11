//! Discord Rich Presence: shows "Mallet" on the user's Discord profile, with the map
//! being edited and how long the editor has been open.
//!
//! A background thread owns the connection to the Discord client. The window sends it
//! the wanted [`Status`] whenever that changes; the thread connects when Discord is
//! running, retries every few seconds when it is not, and clears the activity when
//! presence is turned off.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{Activity, Assets, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

/// The Discord application Mallet's presence belongs to.
const APPLICATION_ID: &str = "1570198929193181314";

/// The art asset uploaded to the application, shown as the large picture.
const LARGE_IMAGE: &str = "mallet";

/// How long to wait before trying to reach Discord again.
const RETRY: Duration = Duration::from_secs(15);

/// What the presence should say, or nothing when it is turned off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The top line, such as `Editing de_dust.vmf`.
    pub details: String,
    /// The second line, such as `Compiling`; empty to leave it out.
    pub state: String,
}

/// The window's side of the presence thread.
pub struct Presence {
    sender: Sender<Option<Status>>,
    last: Option<Option<Status>>,
}

impl Presence {
    /// Starts the presence thread. Nothing is shown until [`Presence::set`] is called
    /// with a status.
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::channel();
        let started = unix_millis();
        let spawned = thread::Builder::new()
            .name("discord-presence".to_owned())
            .spawn(move || run(receiver, started));
        if let Err(error) = spawned {
            eprintln!("Discord presence could not start: {error}");
        }
        Self { sender, last: None }
    }

    /// Shows `status`, or clears the presence when it is `None`. Repeating the same
    /// status does nothing.
    pub fn set(&mut self, status: Option<Status>) {
        if self.last.as_ref() == Some(&status) {
            return;
        }
        let _ = self.sender.send(status.clone());
        self.last = Some(status);
    }
}

/// The connection to the Discord client and what it currently shows.
struct Connection {
    client: Option<DiscordIpcClient>,
    shown: Option<Status>,
}

impl Connection {
    /// Makes the Discord client show `wanted`, connecting first if needed. Returns
    /// whether it now does.
    fn sync(&mut self, wanted: Option<&Status>, started: i64) -> bool {
        if self.shown.as_ref() == wanted && (self.client.is_some() || wanted.is_none()) {
            return true;
        }
        let Some(status) = wanted else {
            if let Some(client) = self.client.as_mut() {
                let _ = client.clear_activity().and_then(|_| client.recv().map(|_| ()));
                let _ = client.close();
            }
            self.client = None;
            self.shown = None;
            return true;
        };
        if self.client.is_none() {
            let mut client = DiscordIpcClient::new(APPLICATION_ID);
            if client.connect().is_err() {
                return false;
            }
            self.client = Some(client);
        }
        let Some(client) = self.client.as_mut() else {
            return false;
        };
        let sent = client
            .set_activity(activity(status, started))
            .and_then(|_| client.recv().map(|_| ()));
        if sent.is_err() {
            self.client = None;
            self.shown = None;
            return false;
        }
        self.shown = Some(status.clone());
        true
    }
}

/// The presence thread: applies each status it receives and retries while Discord is
/// unreachable, until the window goes away.
fn run(receiver: Receiver<Option<Status>>, started: i64) {
    let mut connection = Connection {
        client: None,
        shown: None,
    };
    let mut wanted: Option<Status> = None;
    let mut synced = true;
    loop {
        let message = if synced {
            receiver.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            receiver.recv_timeout(RETRY)
        };
        match message {
            Ok(status) => wanted = status,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(status) = receiver.try_recv() {
            wanted = status;
        }
        synced = connection.sync(wanted.as_ref(), started);
    }
    connection.sync(None, started);
}

/// The activity Discord is sent for `status`.
fn activity(status: &Status, started: i64) -> Activity<'_> {
    let assets = Assets::new()
        .large_image(LARGE_IMAGE)
        .large_text(concat!("Mallet ", env!("CARGO_PKG_VERSION")));
    let activity = Activity::new()
        .details(status.details.as_str())
        .timestamps(Timestamps::new().start(started))
        .assets(assets);
    if status.state.is_empty() {
        activity
    } else {
        activity.state(status.state.as_str())
    }
}

/// The current time as Unix milliseconds, which Discord counts the elapsed time from.
fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
