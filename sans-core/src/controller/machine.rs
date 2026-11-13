//! The Machine thread: board and Camera open, captures and Camera failure
//! polling, kept off the controller thread so they never delay Stop.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;

use super::{ControllerMachine, SETUP_POLL_INTERVAL};
use super::{Inbox, MachineEvent, MachineFactory, SetupBlocker, SetupDiagnostic, SetupState};
use crate::PreparedMachineProfile;

/// Camera blockers then the board's; Ready only when nothing blocks.
pub(super) fn setup_state(
    cameras: Result<Vec<SetupDiagnostic>, Vec<SetupBlocker>>,
    board: &Option<Result<SetupDiagnostic, SetupBlocker>>,
) -> SetupState {
    let (mut diagnostics, mut reasons) = match cameras {
        Ok(diagnostics) => (diagnostics, Vec::new()),
        Err(reasons) => (Vec::new(), reasons),
    };
    match board {
        Some(Ok(diagnostic)) => diagnostics.push(diagnostic.clone()),
        Some(Err(blocker)) => reasons.push(blocker.clone()),
        None => {}
    }
    if reasons.is_empty() {
        SetupState::Ready { diagnostics }
    } else {
        SetupState::Blocked { reasons }
    }
}

pub(super) enum MachineRequest {
    CapturePair,
}

/// Run the factory on its own thread: open the board, then the Cameras,
/// then serve captures and poll for failures until the controller is gone.
/// The Machine never leaves this thread, so it need not be `Send`.
pub(super) fn spawn_machine<Factory: MachineFactory>(
    profile: Arc<PreparedMachineProfile>,
    mut factory: Factory,
    inbox: Sender<Inbox>,
) -> std::io::Result<Sender<MachineRequest>> {
    let (requests, received) = mpsc::channel();
    thread::Builder::new()
        .name("sans-machine".into())
        .spawn(move || {
            struct ReportPanic(Sender<Inbox>);

            impl Drop for ReportPanic {
                fn drop(&mut self) {
                    if thread::panicking() {
                        let _ = self.0.send(Inbox::MachinePanicked);
                    }
                }
            }

            let _report = ReportPanic(inbox.clone());
            if let Some(board) = factory.open_board(&profile) {
                if inbox.send(Inbox::Board(board)).is_err() {
                    return;
                }
            }
            let mut machine = match factory.open(&profile) {
                Ok((machine, diagnostics)) => {
                    if inbox.send(Inbox::Cameras(Ok(diagnostics))).is_err() {
                        return;
                    }
                    machine
                }
                Err(reasons) => {
                    let _ = inbox.send(Inbox::Cameras(Err(reasons)));
                    return;
                }
            };
            loop {
                let sent = match received.recv_timeout(SETUP_POLL_INTERVAL) {
                    Ok(MachineRequest::CapturePair) => {
                        inbox.send(Inbox::Captured(machine.capture_pair()))
                    }
                    Err(RecvTimeoutError::Timeout) => match Factory::poll_machine(&mut machine) {
                        None => Ok(()),
                        Some(event) => {
                            let lost = matches!(event, MachineEvent::Lost(_));
                            let sent = inbox.send(Inbox::Machine(event));
                            if lost {
                                return;
                            }
                            sent
                        }
                    },
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                if sent.is_err() {
                    return;
                }
            }
        })?;
    Ok(requests)
}
