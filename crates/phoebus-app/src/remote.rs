//! A tiny session-bus service for desktop widgets: the up-next queue, and two verbs.
//!
//! MPRIS covers metadata and transport, but it has no queue — souvlaki does not
//! implement `org.mpris.MediaPlayer2.TrackList`, and the Omarchy bar widget
//! (`contrib/omarchy`) wants to *show* what plays next and jump to it. So Phoebus
//! additionally claims `org.phoebus.Phoebus` and serves one interface,
//! `org.phoebus.Queue`, at `/org/phoebus`:
//!
//! | member          | signature  | meaning                                            |
//! |-----------------|------------|----------------------------------------------------|
//! | `Upcoming`      | `() -> s`  | JSON: `{"shuffle": bool, "upcoming": [{title, artist}]}` |
//! | `Jump`          | `(u)`      | play the n-th upcoming row (the drawer's jump)     |
//! | `ToggleShuffle` | `()`       | the player bar's shuffle toggle                    |
//!
//! The shape mirrors [`crate::media_keys`]: a background thread owns the bus
//! connection, commands travel home over a channel and are drained into ordinary
//! [`Action`]s once per frame, and the thread never touches app state. Reads are
//! served from a string snapshot the app republishes whenever the queue changes,
//! so a call never has to wait for a frame.
//!
//! Linux only: the service exists for Linux desktop widgets, and the `dbus` crate
//! is already in the tree there (souvlaki's MPRIS backend). Everywhere else the
//! type is an inert stub.

/// One request a widget sent over the bus.
enum RemoteCmd {
    /// Play the n-th row of the upcoming list.
    Jump(usize),
    /// Flip shuffle.
    ToggleShuffle,
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::{Arc, PoisonError, RwLock};
    use std::time::Duration;

    use crossbeam_channel::{Receiver, Sender};
    use dbus::blocking::Connection;
    use dbus::channel::{MatchingReceiver, Sender as _};
    use dbus::message::MatchRule;

    use super::RemoteCmd;
    use crate::nav::Action;

    /// The bus name the service claims. A restarted Phoebus takes it over.
    const BUS_NAME: &str = "org.phoebus.Phoebus";
    /// The one object the service exports.
    const PATH: &str = "/org/phoebus";
    /// The one interface on it.
    const INTERFACE: &str = "org.phoebus.Queue";

    pub struct Remote {
        rx: Receiver<RemoteCmd>,
        snapshot: Arc<RwLock<String>>,
        /// What was last published, so an unchanged queue costs one string compare.
        last: String,
    }

    impl Remote {
        /// Claim the bus name on a background thread. Failure (no session bus, the
        /// name refused) downgrades to a warning: the service is an optional
        /// convenience for widgets, never worth failing the app over.
        pub fn new(ctx: &egui::Context) -> Remote {
            let (tx, rx) = crossbeam_channel::unbounded::<RemoteCmd>();
            let snapshot: Arc<RwLock<String>> =
                Arc::new(RwLock::new("{\"shuffle\":false,\"upcoming\":[]}".to_string()));
            let served = Arc::clone(&snapshot);
            let ctx = ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("phoebus-remote".to_string())
                .spawn(move || serve(served, tx, ctx));
            if let Err(e) = spawned {
                log::warn!("remote: could not spawn the bus thread: {e}");
            }
            Remote {
                rx,
                snapshot,
                last: String::new(),
            }
        }

        /// Drain everything widgets asked for since the last frame into `out`.
        pub fn poll(&mut self, out: &mut Vec<Action>) {
            for cmd in self.rx.try_iter() {
                out.push(match cmd {
                    RemoteCmd::Jump(i) => Action::QueueJump(i),
                    RemoteCmd::ToggleShuffle => Action::ToggleShuffle,
                });
            }
        }

        /// Publish a fresh `Upcoming` payload. Unchanged text takes no lock.
        pub fn publish(&mut self, json: String) {
            if self.last == json {
                return;
            }
            *self
                .snapshot
                .write()
                .unwrap_or_else(PoisonError::into_inner) = json.clone();
            self.last = json;
        }
    }

    /// The bus thread: claim the name, answer method calls until the connection dies.
    fn serve(snapshot: Arc<RwLock<String>>, tx: Sender<RemoteCmd>, ctx: egui::Context) {
        let conn = match Connection::new_session() {
            Ok(conn) => conn,
            Err(e) => {
                log::warn!("remote: no session bus ({e}); queue service off");
                return;
            }
        };
        if let Err(e) = conn.request_name(BUS_NAME, false, true, true) {
            log::warn!("remote: could not claim {BUS_NAME} ({e}); queue service off");
            return;
        }
        log::info!("remote: serving {INTERFACE} on {BUS_NAME}{PATH}");
        conn.start_receive(
            MatchRule::new_method_call(),
            Box::new(move |msg, conn| {
                handle(&msg, conn, &snapshot, &tx, &ctx);
                true
            }),
        );
        loop {
            if let Err(e) = conn.process(Duration::from_secs(3600)) {
                log::warn!("remote: bus connection lost ({e}); queue service off");
                return;
            }
        }
    }

    /// Answer one method call. Anything not ours is ignored — the match rule is
    /// broad (every method call on the connection), and other paths are not errors.
    fn handle(
        msg: &dbus::Message,
        conn: &Connection,
        snapshot: &Arc<RwLock<String>>,
        tx: &Sender<RemoteCmd>,
        ctx: &egui::Context,
    ) {
        let ours = msg.path().is_some_and(|p| &*p == PATH)
            && msg.interface().is_some_and(|i| &*i == INTERFACE);
        if !ours {
            return;
        }
        let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
        let reply = match member.as_str() {
            "Upcoming" => {
                let json = snapshot
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                msg.method_return().append1(json)
            }
            "Jump" => match msg.read1::<u32>() {
                Ok(index) => {
                    let _ = tx.send(RemoteCmd::Jump(index as usize));
                    // The command is applied by the frame loop; a hidden window
                    // still has to wake up for it, exactly like a media key.
                    ctx.request_repaint();
                    msg.method_return()
                }
                Err(_) => error_reply(msg, "Jump takes one uint32: the upcoming row"),
            },
            "ToggleShuffle" => {
                let _ = tx.send(RemoteCmd::ToggleShuffle);
                ctx.request_repaint();
                msg.method_return()
            }
            _ => error_reply(msg, "unknown member"),
        };
        let _ = conn.send(reply);
    }

    /// A `org.phoebus.Error` reply. Text that will not fit a `CStr` (impossible for
    /// the literals used here) degrades to a plain method return rather than a panic.
    fn error_reply(msg: &dbus::Message, text: &str) -> dbus::Message {
        match std::ffi::CString::new(text) {
            Ok(text) => msg.error(&"org.phoebus.Error".into(), &text),
            Err(_) => msg.method_return(),
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::RemoteCmd;
    use crate::nav::Action;

    /// The inert stub: no bus, nothing to drain, publishing goes nowhere.
    pub struct Remote {
        /// Keeps the enum (and its doc) alive off-Linux without a cfg on it.
        _never: std::marker::PhantomData<RemoteCmd>,
    }

    impl Remote {
        pub fn new(_ctx: &egui::Context) -> Remote {
            Remote {
                _never: std::marker::PhantomData,
            }
        }
        pub fn poll(&mut self, _out: &mut Vec<Action>) {}
        pub fn publish(&mut self, _json: String) {}
    }
}

pub use imp::Remote;

/// The `Upcoming` payload for the current queue: shuffle, and up to `limit`
/// upcoming tracks resolved against the library. Pure, so it is testable without
/// a bus; the app calls it once per frame and [`Remote::publish`] dedupes.
pub fn queue_json(
    library: &phoebus_core::Library,
    queue: &phoebus_core::PlayQueue,
    shuffle: bool,
    limit: usize,
) -> String {
    let upcoming: Vec<serde_json::Value> = queue
        .upcoming(limit)
        .into_iter()
        .filter_map(|next| {
            library.track(next.id).map(|t| {
                serde_json::json!({
                    "title": t.title,
                    "artist": t.artist,
                })
            })
        })
        .collect();
    serde_json::json!({ "shuffle": shuffle, "upcoming": upcoming }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty queue serializes to the same neutral payload the snapshot starts
    /// with, and the JSON is well-formed either way.
    #[test]
    fn queue_json_is_wellformed() {
        let library = phoebus_core::Library::empty("/nonexistent");
        let queue = phoebus_core::PlayQueue::new();
        let json = queue_json(&library, &queue, false, 50);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["shuffle"], false);
        assert!(parsed["upcoming"].as_array().expect("array").is_empty());
    }
}
