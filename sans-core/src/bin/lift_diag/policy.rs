//! Diagnostic commands composed from production lift and cross-board Stop policy.

use std::str::FromStr;
use std::time::Instant;

use sans_core::{
    LiftEvent, LiftOperations, LiftRequest, LiftStatus, LinkEvent, LinkPort, MachineProfile,
    OperationId, Relays, StopCause, StopControl, StopStatus, TelemetrySample, VacuumEvidence,
};

pub(super) const HELP: &str = "home | arm | disarm | move <absolute-mm> | touchdown <amps> [mm/s] | lift <absolute-mm> | cancel | stop | clear-fault | status | quit";

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Command {
    Home,
    Arm,
    Disarm,
    Move(f32),
    Touchdown(f32, Option<f32>),
    Lift(f32),
    Cancel,
    Stop,
    ClearFault,
    Status,
    Quit,
}

impl FromStr for Command {
    type Err = String;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let words: Vec<_> = line.split_whitespace().collect();
        match words.as_slice() {
            ["home"] => Ok(Self::Home),
            ["arm"] => Ok(Self::Arm),
            ["disarm"] => Ok(Self::Disarm),
            ["touchdown", value, speed @ ..] if speed.len() <= 1 => {
                let amps = value
                    .parse::<f32>()
                    .map_err(|_| "touchdown expects current in amps".to_owned())?;
                if !amps.is_finite() || amps <= 0.0 {
                    return Err("touchdown current must be finite and positive amps".into());
                }
                let speed = speed
                    .first()
                    .map(|s| {
                        s.parse::<f32>()
                            .map_err(|_| "touchdown speed expects mm/s".to_owned())
                    })
                    .transpose()?;
                if speed.is_some_and(|s| !s.is_finite() || s <= 0.0) {
                    return Err("touchdown speed must be finite and positive mm/s".into());
                }
                Ok(Self::Touchdown(amps, speed))
            }
            ["cancel"] => Ok(Self::Cancel),
            ["stop"] => Ok(Self::Stop),
            ["clear-fault"] => Ok(Self::ClearFault),
            ["status" | "help"] => Ok(Self::Status),
            ["quit"] => Ok(Self::Quit),
            [name @ ("move" | "lift"), value] => {
                let mm = value.parse::<f32>().map_err(|_| HELP.to_owned())?;
                if !mm.is_finite() {
                    return Err("target must be finite millimetres".into());
                }
                Ok(if *name == "move" {
                    Self::Move(mm)
                } else {
                    Self::Lift(mm)
                })
            }
            _ => Err(HELP.into()),
        }
    }
}

impl Command {
    pub(super) fn urgent(self) -> bool {
        matches!(self, Self::Stop | Self::Quit | Self::Cancel | Self::Disarm)
    }
}

pub(super) struct Diagnostic {
    ops: LiftOperations,
    stop: StopControl,
    vacuum: VacuumEvidence,
    latest: Option<TelemetrySample>,
}

impl Diagnostic {
    /// Called only after the Monospace readiness gate confirms ALL OFF.
    pub(super) fn new(profile: &MachineProfile) -> Self {
        Self {
            ops: LiftOperations::new(profile),
            stop: StopControl::new(profile),
            vacuum: VacuumEvidence::ConfirmedOff,
            latest: None,
        }
    }

    pub(super) fn command(
        &mut self,
        now: Instant,
        command: Command,
        link: &mut LinkPort<'_>,
        relays: &mut Relays<'_>,
    ) -> Result<Option<OperationId>, String> {
        let request = match command {
            Command::Home => LiftRequest::Home,
            Command::Arm => LiftRequest::Arm,
            Command::Disarm => LiftRequest::Disarm,
            Command::Move(position_mm) => LiftRequest::MoveTo { position_mm },
            Command::Touchdown(press_current_a, velocity_limit_mm_s) => LiftRequest::Touchdown {
                press_current_a,
                velocity_limit_mm_s,
                post_turn_page_width_mm: None,
            },
            Command::Lift(position_mm) => {
                let current = self
                    .latest
                    .as_ref()
                    .filter(|sample| sample.epoch() == self.ops.status(now).epoch)
                    .and_then(|sample| sample.fresh_values(now))
                    .and_then(|values| values.position_mm)
                    .ok_or("lift needs a fresh position")?;
                if !position_mm.is_finite() || position_mm <= current {
                    return Err(
                        "lift target must be above the current position, in absolute millimetres"
                            .into(),
                    );
                }
                LiftRequest::MoveTo { position_mm }
            }
            Command::Cancel => {
                if let Some(id) = self.ops.status(now).active {
                    self.ops.cancel(now, id, link);
                }
                return Ok(None);
            }
            Command::Stop | Command::Quit => {
                self.halt(
                    now,
                    StopCause::Requested(format!("diagnostic {command:?}")),
                    link,
                    relays,
                );
                return Ok(None);
            }
            Command::ClearFault => {
                return if self.ops.status(now).latch.is_some() {
                    self.stop
                        .clear(now, &mut self.ops, link)
                        .map(Some)
                        .map_err(|e| format!("{e:?}"))
                } else {
                    self.ops
                        .clear_fault(now, link)
                        .map(Some)
                        .map_err(|e| format!("{e:?}"))
                };
            }
            Command::Status => return Ok(None),
        };
        self.ops
            .start(now, request, self.vacuum, link)
            .map(Some)
            .map_err(|e| format!("{e:?}"))
    }

    pub(super) fn link_event(&mut self, now: Instant, event: &LinkEvent, link: &mut LinkPort<'_>) {
        self.ops.handle_link_event(now, event, link);
        if let LinkEvent::Telemetry(sample) = event {
            self.latest = Some(sample.clone());
        }
    }

    pub(super) fn poll(&mut self, now: Instant, link: &mut LinkPort<'_>) {
        self.ops.poll(now, link);
    }

    pub(super) fn halt(
        &mut self,
        now: Instant,
        cause: StopCause,
        link: &mut LinkPort<'_>,
        relays: &mut Relays<'_>,
    ) {
        self.stop.stop(now, cause, &mut self.ops, link, relays);
    }

    pub(super) fn board_lost(
        &mut self,
        now: Instant,
        link: &mut LinkPort<'_>,
        relays: &mut Relays<'_>,
    ) {
        self.vacuum = VacuumEvidence::NotConfirmedOff;
        self.halt(
            now,
            StopCause::LinkLost("Monospace connection lost".into()),
            link,
            relays,
        );
    }

    /// Observe Stop after every policy call and retain outcomes for logging.
    pub(super) fn settle(&mut self, now: Instant, relays: &mut Relays<'_>) -> Vec<LiftEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.ops.take_event() {
            self.stop.note(&event);
            events.push(event);
        }
        self.stop.observe(now, &self.ops, relays);
        events
    }

    pub(super) fn status(&self, now: Instant) -> (LiftStatus, Option<StopStatus>) {
        (self.ops.status(now), self.stop.status())
    }

    pub(super) fn sample_summary(&self, now: Instant) -> String {
        let Some(sample) = self
            .latest
            .as_ref()
            .filter(|s| s.epoch() == self.ops.status(now).epoch)
        else {
            return "no current-epoch sample".into();
        };
        let age_ms = sample.age(now).as_secs_f64() * 1000.0;
        match sample.fresh_values(now) {
            Some(values) => format!(
                "position_mm={:?} velocity_mm_s={:?} measured_a={:.3} setpoint_a={:.3} age_ms={age_ms:.1}",
                values.position_mm, values.velocity_mm_s, values.iq_measured_a, values.iq_setpoint_a
            ),
            None => format!("stale age_ms={age_ms:.1}; values withheld"),
        }
    }

    pub(super) fn next_wakeup(&self) -> Option<Instant> {
        self.ops
            .next_wakeup()
            .into_iter()
            .chain(self.stop.next_wakeup())
            .min()
    }
}
