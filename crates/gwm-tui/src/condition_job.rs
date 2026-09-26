//! Repair a disk (experimental), run off the render loop.
//!
//! Cycle 1 writes the tracks and reads them back. Any track that isn't perfect
//! — a wrong or missing sector, or one that only read after retries — is
//! written again with every bit flipped and read back, cycle after cycle, until
//! it reads good or the cycle limit runs out. A track that reads good again
//! isn't called repaired until it passes `CONFIRM_PASSES` more cycles in a row
//! (see `gwm_core::disk_test::RepairTracker`). With `erase` on (the default),
//! every write is preceded by an AC erase of those tracks (`gw erase --hfreq`,
//! the closest a drive gets to degaussing): it clears old and off-track signal
//! and spins the disk under the head a couple of extra turns. Still-failing tracks are batched
//! per gw call (see `gwm_core::disk_test::tracks_specs`); per track it's the
//! same write → verify → flip loop.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use gwm_core::disk_test::{self, ConditionEnd, ConditionRound, Geometry, RepairTracker, TrackOutcome, CONFIRM_PASSES};
use gwm_core::read::ReadEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Erasing,
    Writing,
    Reading,
}

enum Msg {
    Step { cycle: u32, step: Step, tracks: usize, confirming: usize },
    Cycle {
        round: ConditionRound,
        outcomes: BTreeMap<(u32, u32), TrackOutcome>,
        relapsed: BTreeSet<(u32, u32)>,
    },
    Done { end: ConditionEnd },
}

pub struct ConditionJob {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    pub format: String,
    pub drive: String,
    pub geom: Geometry,
    pub max_rounds: u32,
    /// AC-erase the tracks before each write.
    pub erase: bool,
    /// Cycle 1 covered the whole disk (the menu's "Repair a disk"), rather than
    /// only the tracks a test had flagged.
    pub full_disk: bool,
    /// The tracks cycle 1 worked on.
    pub start: BTreeSet<(u32, u32)>,
    /// How each finished track ended.
    pub outcomes: BTreeMap<(u32, u32), TrackOutcome>,
    /// Tracks that read good and then failed again while confirming.
    pub relapsed: BTreeSet<(u32, u32)>,
    pub round: u32,
    /// Current step, tracks in it, and how many of those are confirming.
    pub step: Option<(Step, usize, usize)>,
    pub history: Vec<ConditionRound>,
    pub end: Option<ConditionEnd>,
    /// Tracks not good or repaired: failed, or unfinished when it stopped.
    pub left: BTreeSet<(u32, u32)>,
    pub cancelled: bool,
}

impl ConditionJob {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        format: String,
        drive: String,
        geom: Geometry,
        tracks: BTreeSet<(u32, u32)>,
        full_disk: bool,
        max_rounds: u32,
        erase: bool,
        hard_sectors: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let (f, d, t) = (format.clone(), drive.clone(), tracks.clone());
        thread::spawn(move || run(tx, flag, f, d, geom, t, full_disk, max_rounds, erase, hard_sectors));
        ConditionJob {
            rx,
            cancel,
            format,
            drive,
            geom,
            max_rounds,
            erase,
            full_disk,
            left: tracks.clone(),
            start: tracks,
            outcomes: BTreeMap::new(),
            relapsed: BTreeSet::new(),
            round: 0,
            step: None,
            history: Vec::new(),
            end: None,
            cancelled: false,
        }
    }

    /// A finished job from known results, for rendering tests (never touches
    /// gw or the drive).
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn finished_for_test(
        geom: Geometry,
        full_disk: bool,
        start: BTreeSet<(u32, u32)>,
        outcomes: BTreeMap<(u32, u32), TrackOutcome>,
        relapsed: BTreeSet<(u32, u32)>,
        history: Vec<ConditionRound>,
        end: ConditionEnd,
        max_rounds: u32,
    ) -> Self {
        let left = unresolved(&start, &outcomes);
        let (_tx, rx) = mpsc::channel();
        ConditionJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            format: "ibm.360".to_string(),
            drive: "a".to_string(),
            geom,
            max_rounds,
            erase: true,
            full_disk,
            start,
            outcomes,
            relapsed,
            round: history.len() as u32,
            step: None,
            history,
            end: Some(end),
            left,
            cancelled: false,
        }
    }

    pub fn request_cancel(&mut self) {
        self.cancelled = true;
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Drain progress. Returns `true` on the tick the repair ends.
    pub fn pump(&mut self) -> bool {
        let mut ended = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Step { cycle, step, tracks, confirming } => {
                    self.round = cycle;
                    self.step = Some((step, tracks, confirming));
                }
                Msg::Cycle { round, outcomes, relapsed } => {
                    self.left = unresolved(&self.start, &outcomes);
                    self.outcomes = outcomes;
                    self.relapsed = relapsed;
                    self.history.push(round);
                }
                Msg::Done { end } => {
                    self.end = Some(end);
                    ended = true;
                }
            }
        }
        ended
    }

    pub fn ratio(&self) -> f64 {
        (self.history.len() as f64 / self.max_rounds.max(1) as f64).clamp(0.0, 1.0)
    }

    /// Repaired tracks (read good, then confirmed), with the cycle each
    /// started reading good on.
    pub fn repaired(&self) -> BTreeMap<(u32, u32), u32> {
        self.outcomes
            .iter()
            .filter_map(|(&t, o)| match o {
                TrackOutcome::Repaired { cycle } => Some((t, *cycle)),
                _ => None,
            })
            .collect()
    }

    /// Tracks perfect on the first write of a whole-disk run.
    pub fn good_first_time(&self) -> usize {
        self.outcomes.values().filter(|o| **o == TrackOutcome::GoodFirstTime).count()
    }


    /// What it's doing right now, in words.
    pub fn describe_step(&self) -> String {
        match self.step {
            None => "Starting…".to_string(),
            Some((step, n, confirming)) => {
                let tracks = format!("{n} track{}", if n == 1 { "" } else { "s" });
                let what = match (step, self.round) {
                    (Step::Erasing, _) => format!("erasing {tracks}"),
                    (Step::Writing, 1) => format!("writing {tracks}"),
                    (Step::Writing, _) => format!("rewriting {tracks} with the bits flipped"),
                    (Step::Reading, _) => format!("reading {tracks} back"),
                };
                let confirming = if confirming > 0 {
                    format!(" ({confirming} being confirmed, {CONFIRM_PASSES} clean cycles each)")
                } else {
                    String::new()
                };
                format!("Cycle {} (limit {}) — {what}{confirming}", self.round, self.max_rounds)
            }
        }
    }
}

/// Tracks of `start` that are neither good nor repaired.
fn unresolved(start: &BTreeSet<(u32, u32)>, outcomes: &BTreeMap<(u32, u32), TrackOutcome>) -> BTreeSet<(u32, u32)> {
    start
        .iter()
        .copied()
        .filter(|t| !matches!(outcomes.get(t), Some(TrackOutcome::GoodFirstTime | TrackOutcome::Repaired { .. })))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn run(
    tx: Sender<Msg>,
    cancel: Arc<AtomicBool>,
    format: String,
    drive: String,
    geom: Geometry,
    tracks: BTreeSet<(u32, u32)>,
    full_disk: bool,
    max_cycles: u32,
    erase: bool,
    hard_sectors: bool,
) {
    let dir = std::env::temp_dir().join(format!("lubeshop-repair-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let seed = disk_test::fresh_seed();
    let mut tracker = RepairTracker::new(&tracks, full_disk, max_cycles, CONFIRM_PASSES);
    let mut stopped = false;
    // Failing tracks stop at the limit; ones confirming may run CONFIRM_PASSES
    // past it. The hard cap is only a backstop.
    let cap = max_cycles + CONFIRM_PASSES + 1;

    for cycle in 1..=cap {
        if tracker.finished() {
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        let working = tracker.working();
        let confirming = tracker.confirming().len();
        let data = disk_test::round_pattern(&geom, seed, cycle);
        let img = dir.join("cycle.img");
        let mut result = ConditionRound { round: cycle, tracks: working.len(), ..Default::default() };
        if let Err(e) = std::fs::write(&img, &data) {
            result.error = Some(format!("could not stage the data: {e}"));
        }
        let mut bad = Vec::new();
        let mut retried: BTreeSet<(u32, u32)> = BTreeSet::new();

        for (here, spec) in disk_test::tracks_specs(&working) {
            if result.error.is_some() || cancel.load(Ordering::Relaxed) {
                break;
            }
            let step = |s| {
                let _ = tx.send(Msg::Step { cycle, step: s, tracks: here.len(), confirming });
            };

            // AC-erase first: clears old and off-track signal.
            if erase {
                step(Step::Erasing);
                let args = vec![
                    "erase".to_string(),
                    format!("--drive={drive}"),
                    "--hfreq".to_string(),
                    "--revs=2".to_string(),
                    format!("--tracks={spec}"),
                ];
                if let Some(e) = run_gw(&args, &cancel) {
                    result.error = Some(e);
                    break;
                }
            }

            // Write once — no gw verify, the read-back is the judge.
            step(Step::Writing);
            let write = crate::disk_test_job::write_args(&format, &drive, hard_sectors, Some(&spec), &img);
            if let Some(e) = run_gw(&write, &cancel) {
                result.error = Some(e);
                break;
            }

            // Read just these tracks back and compare them.
            step(Step::Reading);
            let back = dir.join("back.img");
            let _ = std::fs::remove_file(&back);
            let args = gwm_core::device::build_read_args(
                &format,
                &drive,
                None,
                hard_sectors,
                false,
                Some(&spec),
                &back.to_string_lossy(),
            );
            let mut failed = None;
            let status = gwm_core::read::run_read_cancellable(&args, cancel.clone(), |ev| match ev {
                ReadEvent::Track { cyl, head, retry: Some(_), .. } | ReadEvent::GaveUp { cyl, head, .. } => {
                    retried.insert((cyl, head));
                }
                ReadEvent::Failed(e) => failed = Some(e),
                _ => {}
            });
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if let Err(e) = status {
                failed = Some(format!("could not run gw: {e}"));
            }
            let got = std::fs::read(&back).unwrap_or_default();
            if got.len() < geom.bytes() {
                result.error = Some(failed.unwrap_or_else(|| "the read-back stopped early".to_string()));
                break;
            }
            bad.extend(disk_test::compare_tracks(&geom, &data, &got, &here));
        }
        if cancel.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        if result.error.is_some() {
            // A gw failure isn't the disk's verdict: stop, don't count it.
            let _ = tx.send(Msg::Cycle { round: result, outcomes: tracker.outcomes(), relapsed: tracker.relapsed_tracks() });
            stopped = true;
            break;
        }

        let failing: BTreeSet<(u32, u32)> = bad
            .iter()
            .map(|s| (s.cyl, s.head))
            .chain(retried.iter().copied())
            .filter(|t| working.contains(t))
            .collect();
        let (good, repaired, relapsed) = tracker.record(cycle, &failing);
        result.bad_sectors = bad.len();
        result.retried = retried.iter().filter(|t| working.contains(t)).count();
        result.healed = good;
        result.confirmed = repaired;
        result.relapsed = relapsed;
        let _ = tx.send(Msg::Cycle { round: result, outcomes: tracker.outcomes(), relapsed: tracker.relapsed_tracks() });
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = tx.send(Msg::Done { end: tracker.end(stopped) });
}

/// Run a gw command to completion; `Some(reason)` if it failed.
fn run_gw(args: &[String], cancel: &Arc<AtomicBool>) -> Option<String> {
    let mut fatal = gwm_core::proc::FatalTracker::default();
    let mut failed = None;
    let status = gwm_core::proc::run_streaming_cancellable(args, cancel.clone(), |line| {
        if let Some(reason) = fatal.note(line) {
            failed = Some(reason);
        } else if line.contains("Command Failed") {
            failed = Some(line.trim().to_string());
        }
    });
    match status {
        Err(e) => Some(format!("could not run gw: {e}")),
        Ok(_) => failed,
    }
}
