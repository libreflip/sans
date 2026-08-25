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

#[derive(Clone, Debug, Eq, PartialEq)]
struct Write {
    priority: RequestPriority,
    line: String,
}

struct FakeWire {
    writes: Arc<Mutex<Vec<Write>>>,
    incoming: mpsc::Receiver<String>,
}

impl LigatureWire for FakeWire {
    fn send(&mut self, request: &LigatureRequest) -> Result<(), LigatureTransportError> {
        self.writes.lock().unwrap().push(Write {
            priority: request.priority,
            line: request.line.clone(),
        });
        Ok(())
    }

    fn try_line(&mut self) -> Result<Option<(ConnectionEpoch, String)>, LigatureTransportError> {
        match self.incoming.try_recv() {
            Ok(line) => Ok(Some((ConnectionEpoch(1), line))),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(LigatureTransportError::Closed),
        }
    }
}

struct FakeFactory {
    wire: FakeWire,
    query: &'static str,
}

impl MachineFactory for FakeFactory {
    type Machine = LigatureMachine<FakeWire>;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<Self::Machine, Vec<sans_core::SetupBlocker>> {
        let client = LigatureClient::from_query(self.wire, self.query).unwrap();
        Ok(LigatureMachine::new(client))
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
            wire: FakeWire {
                writes: Arc::clone(&writes),
                incoming: incoming_receiver,
            },
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
fn malformed_completion_changes_setup_to_transport_fault() {
    let (controller, incoming, _writes) = controller(READY);
    controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    controller
        .send(ControllerIntent::Ligature(LigatureCommand::Home))
        .unwrap();
    incoming
        .send("done G0 Z:-2.000 STATE:READY TRUST:1".into())
        .unwrap();

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
