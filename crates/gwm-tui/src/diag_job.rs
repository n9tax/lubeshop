//! The live drive diagnostic, hosted as a screen.
//!
//! Longer-lived than the other jobs here: instead of running once and
//! reporting an outcome, this keeps a `gw diag --batch` session open with the
//! spindle turning, absorbing a reading every half second until the user
//! leaves the screen. [`pump`] drains whatever has arrived since the last
//! frame, so the render loop never waits on the drive.

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use gwm_core::diag::{self, DiagCommand, DiagEvent, DiagHello, DiagOptions, DiagStatus};

/// How many recent RPM samples to keep for the trend strip. About 15 seconds
/// at one reading every half second — enough to see a spindle settle after
/// spin-up, or wobble, without the strip becoming a wall of history.
const RPM_HISTORY: usize = 30;

/// How many notices to keep. They arrive rarely (a seek failure, a
/// recalibration), and only the last few are worth screen space.
const MAX_NOTICES: usize = 4;

pub struct DiagJob {
    session: Option<diag::DiagSession>,
    rx: Receiver<DiagEvent>,
    /// Kept so the session can be restarted after a device reset.
    cmd: String,
    opts: DiagOptions,
    /// Whether the one automatic reset-and-retry has already been spent.
    recovered: bool,
    /// The settings the tool confirmed, once it has started up.
    pub hello: Option<DiagHello>,
    /// Most recent reading; `None` until the first one lands.
    pub status: Option<DiagStatus>,
    /// Recent RPM readings, oldest first. `None` marks a tick with no
    /// reading, so a dropout shows as a gap rather than being smoothed over.
    pub rpm_history: VecDeque<Option<f64>>,
    /// Recent notices, newest last: `(is_error, text)`.
    pub notices: VecDeque<(bool, String)>,
    /// Set when the session ends abnormally. The screen stays up showing the
    /// last reading and this reason, rather than vanishing.
    pub failed: Option<String>,
    /// True once the tool has acknowledged shutdown.
    pub finished: bool,
    /// What we last asked for, so the screen's own toggles set an absolute
    /// value instead of flipping a stale one. Seeded from what the session
    /// starts with: selected, motor on, density low.
    pub want_motor: bool,
    pub want_select: bool,
    pub want_density: bool,
    pub want_head: u8,
}

impl DiagJob {
    /// Open a session. `cmd` is the diag-capable `gw` to run.
    pub fn start(cmd: &str, opts: &DiagOptions) -> Result<Self, String> {
        let (session, rx) = diag::start(cmd, opts).map_err(|e| {
            format!("could not start the diagnostic ({cmd}): {e}")
        })?;
        Ok(Self {
            session: Some(session),
            rx,
            cmd: cmd.to_string(),
            opts: opts.clone(),
            recovered: false,
            hello: None,
            status: None,
            rpm_history: VecDeque::with_capacity(RPM_HISTORY),
            notices: VecDeque::new(),
            failed: None,
            finished: false,
            want_motor: true,
            want_select: true,
            want_density: false,
            want_head: 0,
        })
    }

    /// Drain everything that has arrived. Returns `true` if anything changed,
    /// so the caller only redraws when there is something new to show.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.rx.try_recv() {
            changed = true;
            self.absorb(event);
        }
        if self.needs_recovery() {
            self.recover();
            changed = true;
        }
        changed
    }

    /// Whether this looks like a device left mid-command by an earlier session
    /// that was killed rather than shut down.
    ///
    /// The tell is dying *before the first reading ever arrives*: a session
    /// that has been streaming and then fails has hit something real (unplugged
    /// device, tool crash) that a reset would only paper over. Once per job,
    /// so a genuinely broken setup reports its error instead of looping.
    fn needs_recovery(&self) -> bool {
        self.failed.is_some() && self.status.is_none() && !self.recovered
    }

    /// Reset the device and start over once.
    fn recover(&mut self) {
        self.recovered = true;
        if let Some(session) = self.session.take() {
            session.shutdown();
        }
        diag::reset_device(&self.cmd);
        match diag::start(&self.cmd, &self.opts) {
            Ok((session, rx)) => {
                self.session = Some(session);
                self.rx = rx;
                self.failed = None;
                self.finished = false;
                self.notices.push_back((
                    false,
                    "Device was in a stuck state — reset it and started over."
                        .to_string(),
                ));
            }
            // Leave the original failure showing: it says more about what is
            // wrong than "the retry also failed" would.
            Err(_) => self.finished = true,
        }
    }

    /// Fold one event into the visible state. Split out from [`pump`] so a
    /// screen can be rendered from a scripted event stream without a device.
    pub fn absorb(&mut self, event: DiagEvent) {
        match event {
            DiagEvent::Hello(hello) => self.hello = Some(*hello),
            DiagEvent::Status(status) => {
                // Only record RPM while the motor is meant to be running: a
                // deliberate motor-off is not a dropout, and charting it as
                // one would cry wolf.
                if status.motor {
                    if self.rpm_history.len() == RPM_HISTORY {
                        self.rpm_history.pop_front();
                    }
                    self.rpm_history.push_back(status.rpm);
                }
                self.status = Some(*status);
            }
            DiagEvent::Notice { is_error, text } => {
                if self.notices.len() == MAX_NOTICES {
                    self.notices.pop_front();
                }
                self.notices.push_back((is_error, text));
            }
            DiagEvent::Bye => self.finished = true,
            DiagEvent::Failed(reason) => {
                self.failed = Some(reason);
                self.finished = true;
            }
        }
    }

    /// Send a command, recording the intent for the state-setting ones so the
    /// screen's toggles stay in step with what we asked the drive to do.
    pub fn send(&mut self, cmd: DiagCommand) {
        match cmd {
            DiagCommand::Motor(v) => self.want_motor = v,
            DiagCommand::Select(v) => self.want_select = v,
            DiagCommand::Density(v) => self.want_density = v,
            DiagCommand::Head(n) => self.want_head = n,
            _ => {}
        }
        if let Some(session) = self.session.as_ref() {
            if !session.send(cmd) {
                self.failed
                    .get_or_insert_with(|| "the diagnostic stopped responding".to_string());
                self.finished = true;
            }
        }
    }

    /// Whether the session is still usable for commands.
    pub fn is_live(&self) -> bool {
        !self.finished && self.session.is_some()
    }

    /// End the session and wait for the tool to let go of the drive. Called
    /// when the user leaves the screen.
    pub fn stop(&mut self) {
        if let Some(session) = self.session.take() {
            session.shutdown();
        }
        self.finished = true;
    }
}

#[cfg(test)]
impl DiagJob {
    /// A job with no child process behind it, for feeding scripted events to
    /// [`absorb`] and rendering the result.
    pub fn detached_for_test() -> Self {
        let (_tx, rx) = std::sync::mpsc::channel();
        Self {
            session: None,
            rx,
            cmd: "gw".to_string(),
            opts: DiagOptions::default(),
            // Nothing to restart in a detached job, so never try.
            recovered: true,
            hello: None,
            status: None,
            rpm_history: VecDeque::new(),
            notices: VecDeque::new(),
            failed: None,
            finished: false,
            want_motor: true,
            want_select: true,
            want_density: false,
            want_head: 0,
        }
    }
}

impl Drop for DiagJob {
    /// Never leave a spindle turning because a screen went away: whatever
    /// path drops this, the drive gets released.
    fn drop(&mut self) {
        self.stop();
    }
}
