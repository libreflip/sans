use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::thread;

use super::*;

fn acknowledge_readiness(reader: &mut impl BufRead, writer: &mut impl Write) {
    for expected in ["ALL OFF\n", "PRESS STOP\n"] {
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        assert_eq!(command, expected);
        writer.write_all(b"OK\n").unwrap();
    }
}

#[test]
fn connection_waits_for_exact_all_off_then_press_stop_acknowledgements() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let (release_sender, release_receiver) = mpsc::channel();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        release_receiver.recv().unwrap();
    });

    let connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();

    assert!(connection.client.is_usable());
    release_sender.send(()).unwrap();
    board_thread.join().unwrap();
}

#[test]
fn firmware_error_during_gate_returns_no_client() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        (&board).write_all(b"ERR RELAY_FAULT\n").unwrap();
        command
    });

    let result = MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100));

    assert!(matches!(result, Err(HwError::Firmware(reason)) if reason == "RELAY_FAULT"));
    assert_eq!(board_thread.join().unwrap(), "ALL OFF\n");
}

#[test]
fn expired_device_open_deadline_returns_no_client() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();

    let result = MonospaceClient::connect_streams_before(
        host_reader,
        host,
        Duration::from_millis(100),
        Some(Instant::now()),
    );
    drop(board);

    assert!(matches!(result, Err(HwError::DeviceOpenTimeout)));
}

#[test]
fn unsolicited_response_reports_fault_and_poisons_idle_correlation() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let (send_unsolicited, receive_unsolicited) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        receive_unsolicited.recv().unwrap();
        writer.write_all(b"OK\n").unwrap();
        release_receiver.recv().unwrap();
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();
    send_unsolicited.send(()).unwrap();

    let fault = connection
        .events
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        fault.kind,
        MonospaceEventKind::Fault(MonospaceFault::UnexpectedReply("OK".into()))
    );
    let epoch = connection.client.epoch().get();
    match connection.client.set_fan(true) {
        Err(HwError::UnexpectedReply(reply)) => assert_eq!(reply, "OK"),
        Err(HwError::Poisoned(poisoned_epoch)) => assert_eq!(poisoned_epoch, epoch),
        result => panic!("expected the unsolicited reply or poisoned epoch, got {result:?}"),
    }
    assert!(!connection.client.is_usable());
    release_sender.send(()).unwrap();
    board_thread.join().unwrap();
}

#[test]
fn pressure_can_coalesce_while_button_and_unknown_events_keep_their_types() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let (release_sender, release_receiver) = mpsc::channel();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        for sample in 0..100 {
            writeln!(writer, "PRESS {}", 1_000.0 + sample as f32).unwrap();
        }
        writer.write_all(b"EVENT BUTTON PRESSED\n").unwrap();
        writer.write_all(b"EVENT BUTTON RELEASED\n").unwrap();
        release_receiver.recv().unwrap();
    });
    let connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();

    let mut pressure_count = 0;
    let mut kinds = Vec::new();
    while kinds.len() < 2 {
        let event = connection
            .events
            .recv_timeout(Duration::from_millis(500))
            .unwrap();
        match event.kind {
            MonospaceEventKind::Pressure(_) => pressure_count += 1,
            kind => kinds.push(kind),
        }
    }

    assert!(pressure_count < 100);
    assert_eq!(
        kinds,
        [
            MonospaceEventKind::ButtonPressed,
            MonospaceEventKind::UnknownEvent("BUTTON RELEASED".into())
        ]
    );
    assert_eq!(
        connection
            .events
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
            .kind,
        MonospaceEventKind::Pressure(1_099.0)
    );
    release_sender.send(()).unwrap();
    board_thread.join().unwrap();
}

#[test]
fn timeout_poisons_connection_and_late_reply_cannot_satisfy_new_work() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let (release_board, wait_for_timeout) = mpsc::channel();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        wait_for_timeout.recv().unwrap();
        let mut shutdown = String::new();
        reader.read_line(&mut shutdown).unwrap();
        writer.write_all(b"OK\n").unwrap();
        [command, shutdown]
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(20)).unwrap();

    assert!(matches!(
        connection.client.set_vacuum(true),
        Err(HwError::ReplyTimeout)
    ));
    release_board.send(()).unwrap();
    let fault = connection
        .events
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        fault.kind,
        MonospaceEventKind::Fault(MonospaceFault::ReplyTimeout)
    );
    assert!(matches!(
        connection.client.set_fan(true),
        Err(HwError::Poisoned(_))
    ));
    assert_eq!(board_thread.join().unwrap(), ["VACUUM ON\n", "ALL OFF\n"]);
}

#[test]
fn timeout_closes_both_connection_halves() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    host_reader
        .set_read_timeout(Some(Duration::from_millis(10)))
        .unwrap();
    let board_thread = thread::spawn(move || {
        let board_reader = board.try_clone().unwrap();
        board_reader
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut reader = BufReader::new(board_reader);
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        let mut shutdown = String::new();
        reader.read_line(&mut shutdown).unwrap();
        let mut trailing = String::new();
        (
            [command, shutdown],
            reader.read_line(&mut trailing).unwrap(),
        )
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(30)).unwrap();

    assert!(matches!(
        connection.client.set_vacuum(true),
        Err(HwError::ReplyTimeout)
    ));
    let (commands, bytes_after_retirement) = board_thread.join().unwrap();
    assert_eq!(commands, ["VACUUM ON\n", "ALL OFF\n"]);
    assert_eq!(bytes_after_retirement, 0);
}

#[test]
fn observed_disconnect_is_forwarded_after_timeout_fault() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let (release_board, wait_for_timeout) = mpsc::channel();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        wait_for_timeout.recv().unwrap();
        let mut shutdown = String::new();
        reader.read_line(&mut shutdown).unwrap();
        [command, shutdown]
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(20)).unwrap();

    assert!(matches!(
        connection.client.set_vacuum(true),
        Err(HwError::ReplyTimeout)
    ));
    assert_eq!(
        connection
            .events
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
            .kind,
        MonospaceEventKind::Fault(MonospaceFault::ReplyTimeout)
    );
    release_board.send(()).unwrap();
    assert_eq!(
        connection
            .events
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
            .kind,
        MonospaceEventKind::Disconnected
    );
    assert_eq!(board_thread.join().unwrap(), ["VACUUM ON\n", "ALL OFF\n"]);
}

#[test]
fn event_arriving_after_retirement_is_logged_but_not_forwarded() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let (release_board, wait_for_timeout) = mpsc::channel();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        wait_for_timeout.recv().unwrap();
        let mut shutdown = String::new();
        reader.read_line(&mut shutdown).unwrap();
        writer.write_all(b"EVENT BUTTON PRESSED\n").unwrap();
        [command, shutdown]
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(20)).unwrap();

    assert!(matches!(
        connection.client.set_vacuum(true),
        Err(HwError::ReplyTimeout)
    ));
    assert_eq!(
        connection
            .events
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
            .kind,
        MonospaceEventKind::Fault(MonospaceFault::ReplyTimeout)
    );
    release_board.send(()).unwrap();
    assert_eq!(board_thread.join().unwrap(), ["VACUUM ON\n", "ALL OFF\n"]);
    assert!(matches!(
        connection.events.try_recv(),
        Err(TryRecvError::Empty | TryRecvError::Disconnected)
    ));
}

#[test]
fn malformed_frame_poisons_the_connection() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        for response in [b"OK\n".as_slice(), b"OK\n", b"not a frame\n"] {
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            writer.write_all(response).unwrap();
        }
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();

    assert!(matches!(
        connection.client.set_vacuum(true),
        Err(HwError::MalformedFrame(frame)) if frame == "not a frame"
    ));
    let event = connection
        .events
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        event.kind,
        MonospaceEventKind::Fault(MonospaceFault::MalformedFrame("not a frame".into()))
    );
    assert!(!connection.client.is_usable());
    board_thread.join().unwrap();
}

#[test]
fn empty_frame_poisons_the_connection() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        for response in [b"OK\n".as_slice(), b"OK\n", b"\n"] {
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            writer.write_all(response).unwrap();
        }
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();

    assert!(matches!(
        connection.client.set_vacuum(true),
        Err(HwError::MalformedFrame(frame)) if frame.is_empty()
    ));
    assert_eq!(
        connection
            .events
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
            .kind,
        MonospaceEventKind::Fault(MonospaceFault::MalformedFrame(String::new()))
    );
    board_thread.join().unwrap();
}

#[test]
fn unexpected_typed_reply_poisons_the_connection() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let (release_sender, release_receiver) = mpsc::channel();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        for _ in 0..3 {
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            writer.write_all(b"OK\n").unwrap();
        }
        release_receiver.recv().unwrap();
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();

    assert!(matches!(
        connection.client.press_once(),
        Err(HwError::UnexpectedReply(reply)) if reply == "OK"
    ));
    let event = connection
        .events
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        event.kind,
        MonospaceEventKind::Fault(MonospaceFault::UnexpectedReply("OK".into()))
    );
    assert!(!connection.client.is_usable());
    release_sender.send(()).unwrap();
    board_thread.join().unwrap();
}

#[test]
fn firmware_err_is_reported_without_poisoning_correlation() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let (release_sender, release_receiver) = mpsc::channel();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        for response in [b"OK\n".as_slice(), b"OK\n", b"ERR RELAY_FAULT\n", b"OK\n"] {
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            writer.write_all(response).unwrap();
        }
        release_receiver.recv().unwrap();
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();

    assert!(matches!(
        connection.client.set_vacuum(true),
        Err(HwError::Firmware(reason)) if reason == "RELAY_FAULT"
    ));
    assert!(connection.client.set_fan(false).is_ok());
    assert!(connection.client.is_usable());
    release_sender.send(()).unwrap();
    board_thread.join().unwrap();
}

#[test]
fn disconnect_is_forwarded_and_poisons_the_connection() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
    });
    let mut connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();

    assert!(matches!(
        connection.client.set_vacuum(true),
        Err(HwError::Disconnected)
    ));
    let event = connection
        .events
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    assert_eq!(event.kind, MonospaceEventKind::Disconnected);
    assert!(!connection.client.is_usable());
    board_thread.join().unwrap();
    assert_eq!(
        connection.events.recv_timeout(Duration::from_millis(100)),
        Err(RecvTimeoutError::Disconnected)
    );
}

#[test]
fn ambiguous_urgent_write_retires_the_response_fifo() {
    let (host, board) = UnixStream::pair().unwrap();
    let host_reader = host.try_clone().unwrap();
    let board_thread = thread::spawn(move || {
        let mut reader = BufReader::new(board.try_clone().unwrap());
        let mut writer = board;
        acknowledge_readiness(&mut reader, &mut writer);
        let mut urgent_command = String::new();
        reader.read_line(&mut urgent_command).unwrap();
        urgent_command
    });
    let connection =
        MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100)).unwrap();
    let urgent = connection.client.urgent_writer();

    assert!(matches!(
        urgent.all_off(),
        Err(HwError::AmbiguousUrgentWrite)
    ));
    let event = connection
        .events
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        event.kind,
        MonospaceEventKind::Fault(MonospaceFault::AmbiguousUrgentWrite)
    );
    assert!(!connection.client.is_usable());
    assert_eq!(board_thread.join().unwrap(), "ALL OFF\n");
}

#[test]
fn reconnect_uses_a_fresh_epoch() {
    fn open_ready_connection() -> (
        MonospaceConnection,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let (host, board) = UnixStream::pair().unwrap();
        let host_reader = host.try_clone().unwrap();
        let (release_sender, release_receiver) = mpsc::channel();
        let board_thread = thread::spawn(move || {
            let mut reader = BufReader::new(board.try_clone().unwrap());
            let mut writer = board;
            acknowledge_readiness(&mut reader, &mut writer);
            release_receiver.recv().unwrap();
        });
        let connection =
            MonospaceClient::connect_streams(host_reader, host, Duration::from_millis(100))
                .unwrap();
        (connection, release_sender, board_thread)
    }

    let (first, first_release, first_board) = open_ready_connection();
    let first_epoch = first.client.epoch();
    drop(first);
    first_release.send(()).unwrap();
    first_board.join().unwrap();

    let (second, second_release, second_board) = open_ready_connection();
    let second_epoch = second.client.epoch();

    assert_ne!(first_epoch, second_epoch);
    assert!(second_epoch.get() > first_epoch.get());
    second_release.send(()).unwrap();
    second_board.join().unwrap();
}
