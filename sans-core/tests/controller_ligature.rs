//! Fake-wire tests for Ligature behavior at the controller intent and snapshot boundary.

use std::fs;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use sans_core::{
    bootstrap, ConnectionEpoch, ControllerEvent, ControllerIntent, LigatureClient, LigatureCommand,
    LigatureEvent, LigatureMachine, LigaturePosition, LigatureRequest, LigatureState,
    LigatureTransportError, LigatureWire, MachineFactory, MachineScreen, PositionTrust,
    PreparedMachineProfile, RequestPriority, SetupState,
};

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");
const READY: &str = "state STATE:READY TRUST:1 Z:-2.000 VEL:0.000 IQ:0.000 \
                    PRESS:? ENDSTOP:0 PWM:ACTIVE ACTIVE:NONE FAULT:NONE \
                    RUNTIME_MODIFIED:0";
const UNCOMMISSIONED: &str = "state STATE:COMMISSIONING_ONLY TRUST:0 Z:? VEL:0.000 \
                             IQ:0.000 PRESS:? ENDSTOP:0 PWM:OFF ACTIVE:NONE \
                             FAULT:NONE RUNTIME_MODIFIED:0";
const HOMING: &str = "state STATE:HOMING TRUST:0 Z:? VEL:-1.000 IQ:0.400 \
                      PRESS:? ENDSTOP:0 PWM:ACTIVE ACTIVE:G28 FAULT:NONE \
                      RUNTIME_MODIFIED:0";

#[derive(Clone, Debug, Eq, PartialEq)]
struct Write {
    priority: RequestPriority,
    line: String,
}

struct FakeWire {
    writes: Arc<Mutex<Vec<Write>>>,
    incoming: mpsc::Receiver<String>,
    epoch: ConnectionEpoch,
}

impl LigatureWire for FakeWire {
    fn set_epoch(&mut self, epoch: ConnectionEpoch) {
        self.epoch = epoch;
    }

    fn send(&mut self, request: &LigatureRequest) -> Result<(), LigatureTransportError> {
        self.writes.lock().unwrap().push(Write {
            priority: request.priority,
            line: request.line.clone(),
        });
        Ok(())
    }

    fn query_current_state(&mut self) -> Result<(), LigatureTransportError> {
        self.writes.lock().unwrap().push(Write {
            priority: RequestPriority::Ordinary,
            line: "?".into(),
        });
        Ok(())
    }

    fn try_line(&mut self) -> Result<Option<(ConnectionEpoch, String)>, LigatureTransportError> {
        match self.incoming.try_recv() {
            Ok(line) => Ok(Some((self.epoch, line))),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(LigatureTransportError::Closed),
        }
    }

    fn shutdown(self) -> Result<(), LigatureTransportError> {
        self.writes.lock().unwrap().push(Write {
            priority: RequestPriority::Ordinary,
            line: "<shutdown>".into(),
        });
        Ok(())
    }
}

struct FakeFactory {
    wire: Option<FakeWire>,
    query: &'static str,
}

struct ReconnectFactory {
    wire: Option<FakeWire>,
    replacement: Option<FakeWire>,
}

impl MachineFactory for FakeFactory {
    type Machine = LigatureMachine<FakeWire>;

    fn open(
        &mut self,
        _profile: &PreparedMachineProfile,
    ) -> Result<Self::Machine, Vec<sans_core::SetupBlocker>> {
        let client = LigatureClient::from_query(self.wire.take().unwrap(), self.query).unwrap();
        Ok(LigatureMachine::new(client))
    }
}

impl MachineFactory for ReconnectFactory {
    type Machine = LigatureMachine<FakeWire>;

    fn open(
        &mut self,
        _profile: &PreparedMachineProfile,
    ) -> Result<Self::Machine, Vec<sans_core::SetupBlocker>> {
        let client = LigatureClient::from_query(self.wire.take().unwrap(), READY).unwrap();
        let mut replacement = self.replacement.take();
        Ok(LigatureMachine::with_reconnect(client, move || {
            replacement
                .take()
                .map(|wire| (wire, READY.into()))
                .ok_or(LigatureTransportError::Closed)
        }))
    }
}

fn controller(
    query: &'static str,
) -> (
    sans_core::ControllerHandle,
    mpsc::Sender<String>,
    Arc<Mutex<Vec<Write>>>,
) {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let (incoming_sender, incoming_receiver) = mpsc::channel();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let handle = bootstrap(
        Some(&profile_path),
        FakeFactory {
            wire: Some(FakeWire {
                writes: Arc::clone(&writes),
                incoming: incoming_receiver,
                epoch: ConnectionEpoch(1),
            }),
            query,
        },
    )
    .unwrap();
    (handle, incoming_sender, writes)
}

#[test]
fn controller_routes_operation_lifecycle_and_priority_cancel() {
    let (controller, incoming, writes) = controller(READY);
    let setup = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(setup.screen, MachineScreen::Setup(SetupState::Ready));
    let status = setup.ligature.unwrap();
    assert_eq!(status.state, LigatureState::Ready);
    assert_eq!(status.position_trust, PositionTrust::Trusted);
    assert_eq!(status.position, LigaturePosition::Known(-2.0));

    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Home))
        .unwrap();
    incoming.send("ok G28".into()).unwrap();
    assert!(matches!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::Ligature(LigatureEvent::Accepted(_))
    ));

    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Cancel))
        .unwrap();
    incoming
        .send("done M53 CANCELLED:G28 Z:KNOWN STATE:READY TRUST:1".into())
        .unwrap();
    assert!(matches!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::Ligature(LigatureEvent::Cancelled { .. })
    ));
    incoming.send(READY.into()).unwrap();
    assert!(matches!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::Ligature(LigatureEvent::Status(_))
    ));
    let refreshed = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(refreshed.ligature.unwrap().state, LigatureState::Ready);

    assert_eq!(
        *writes.lock().unwrap(),
        vec![
            Write {
                priority: RequestPriority::Ordinary,
                line: "G28".into(),
            },
            Write {
                priority: RequestPriority::Urgent,
                line: "M53".into(),
            },
            Write {
                priority: RequestPriority::Ordinary,
                line: "?".into(),
            },
        ]
    );
    controller.send(ControllerIntent::Exit).unwrap();
}

#[test]
fn controller_exposes_uncommissioned_as_connected_and_scan_disabled() {
    let (controller, _incoming, _writes) = controller(UNCOMMISSIONED);
    let setup = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        setup.screen,
        MachineScreen::Setup(SetupState::Uncommissioned)
    );
    assert_eq!(
        setup.ligature.unwrap().state,
        LigatureState::CommissioningOnly
    );

    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Home))
        .unwrap();
    assert_eq!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::LigatureRejected(sans_core::LigatureSessionError::CommissioningOnly)
    );
    controller.send(ControllerIntent::Exit).unwrap();
}

#[test]
fn controller_rejects_a_second_ordinary_operation_instead_of_queueing_it() {
    let (controller, _incoming, writes) = controller(READY);
    controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();

    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Home))
        .unwrap();
    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Arm))
        .unwrap();

    assert_eq!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::LigatureRejected(sans_core::LigatureSessionError::Busy)
    );
    assert_eq!(
        *writes.lock().unwrap(),
        vec![Write {
            priority: RequestPriority::Ordinary,
            line: "G28".into(),
        }]
    );
    controller.send(ControllerIntent::Exit).unwrap();
}

#[test]
fn malformed_completion_changes_setup_to_transport_fault() {
    let (controller, incoming, _writes) = controller(READY);
    controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Home))
        .unwrap();
    incoming.send("ok G28".into()).unwrap();
    assert!(matches!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::Ligature(LigatureEvent::Accepted(_))
    ));
    incoming.send("done G28".into()).unwrap();

    assert!(matches!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::LigatureTransportFault(_)
    ));
    let fault = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        fault.screen,
        MachineScreen::Setup(SetupState::TransportFault { .. })
    ));
    assert!(fault.ligature.is_none());
    controller.send(ControllerIntent::Exit).unwrap();
}

#[test]
fn reconnect_tags_new_wire_with_the_new_connection_epoch() {
    let (first_sender, first_receiver) = mpsc::channel();
    let first_writes = Arc::new(Mutex::new(Vec::new()));
    let mut client = LigatureClient::from_query(
        FakeWire {
            writes: Arc::clone(&first_writes),
            incoming: first_receiver,
            epoch: ConnectionEpoch(1),
        },
        READY,
    )
    .unwrap();
    client.begin(LigatureCommand::Home).unwrap();

    let (second_sender, second_receiver) = mpsc::channel();
    let second_writes = Arc::new(Mutex::new(Vec::new()));
    let closed_before_open = Arc::clone(&first_writes);
    let reconnect = client
        .reconnect_with(|| {
            assert_eq!(
                closed_before_open.lock().unwrap().last().unwrap().line,
                "<shutdown>"
            );
            Ok((
                FakeWire {
                    writes: second_writes,
                    incoming: second_receiver,
                    epoch: ConnectionEpoch(1),
                },
                READY.into(),
            ))
        })
        .unwrap();
    assert_eq!(reconnect.epoch, ConnectionEpoch(2));
    let current = client.begin(LigatureCommand::Home).unwrap();
    assert_eq!(current.epoch, ConnectionEpoch(2));
    second_sender.send("ok G28".into()).unwrap();

    assert!(matches!(
        client.try_event().unwrap(),
        Some(LigatureEvent::Accepted(operation_id)) if operation_id == current.operation_id
    ));
    drop(first_sender);
}

#[test]
fn opening_on_active_firmware_issues_priority_stop() {
    let (_incoming_sender, incoming_receiver) = mpsc::channel();
    let writes = Arc::new(Mutex::new(Vec::new()));

    let client = LigatureClient::from_query(
        FakeWire {
            writes: Arc::clone(&writes),
            incoming: incoming_receiver,
            epoch: ConnectionEpoch(1),
        },
        HOMING,
    )
    .unwrap();

    assert_eq!(client.session().status().state, LigatureState::Homing);
    assert_eq!(
        *writes.lock().unwrap(),
        vec![Write {
            priority: RequestPriority::Urgent,
            line: "M112".into(),
        }]
    );
}

#[test]
fn reconnect_stops_firmware_work_orphaned_by_the_old_epoch() {
    let (_first_sender, first_receiver) = mpsc::channel();
    let mut client = LigatureClient::from_query(
        FakeWire {
            writes: Arc::new(Mutex::new(Vec::new())),
            incoming: first_receiver,
            epoch: ConnectionEpoch(1),
        },
        READY,
    )
    .unwrap();
    let (second_sender, second_receiver) = mpsc::channel();
    let second_writes = Arc::new(Mutex::new(Vec::new()));

    let reconnect = client
        .reconnect_with(|| {
            Ok((
                FakeWire {
                    writes: Arc::clone(&second_writes),
                    incoming: second_receiver,
                    epoch: ConnectionEpoch(1),
                },
                HOMING.into(),
            ))
        })
        .unwrap();
    assert_eq!(reconnect.epoch, ConnectionEpoch(2));
    assert_eq!(
        *second_writes.lock().unwrap(),
        vec![Write {
            priority: RequestPriority::Urgent,
            line: "M112".into(),
        }]
    );

    second_sender
        .send("done M112 CANCELLED:G28 Z:? STATE:FAULT TRUST:0".into())
        .unwrap();
    assert!(matches!(
        client.try_event().unwrap(),
        Some(LigatureEvent::Completed { terminal, .. })
            if terminal.command.as_str() == "M112"
    ));
    assert_eq!(
        *second_writes.lock().unwrap(),
        vec![
            Write {
                priority: RequestPriority::Urgent,
                line: "M112".into(),
            },
            Write {
                priority: RequestPriority::Ordinary,
                line: "?".into(),
            },
        ]
    );
}

#[test]
fn controller_recovers_from_transport_fault_with_a_fresh_connection() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let (first_sender, first_receiver) = mpsc::channel();
    let (second_sender, second_receiver) = mpsc::channel();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let controller = bootstrap(
        Some(&profile_path),
        ReconnectFactory {
            wire: Some(FakeWire {
                writes: Arc::clone(&writes),
                incoming: first_receiver,
                epoch: ConnectionEpoch(1),
            }),
            replacement: Some(FakeWire {
                writes,
                incoming: second_receiver,
                epoch: ConnectionEpoch(1),
            }),
        },
    )
    .unwrap();
    controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();

    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Home))
        .unwrap();
    first_sender.send("ok G28".into()).unwrap();
    controller
        .recv_event_timeout(Duration::from_secs(1))
        .unwrap();
    first_sender.send("done G28".into()).unwrap();
    assert!(matches!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::LigatureTransportFault(_)
    ));
    controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();

    controller
        .send(ControllerIntent::ReconnectLigature)
        .unwrap();
    let recovered = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(recovered.screen, MachineScreen::Setup(SetupState::Ready));
    assert_eq!(recovered.ligature.unwrap().state, LigatureState::Ready);

    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Home))
        .unwrap();
    second_sender.send("ok G28".into()).unwrap();
    assert!(matches!(
        controller
            .recv_event_timeout(Duration::from_secs(1))
            .unwrap(),
        ControllerEvent::Ligature(LigatureEvent::Accepted(_))
    ));
    controller.send(ControllerIntent::Exit).unwrap();
}
