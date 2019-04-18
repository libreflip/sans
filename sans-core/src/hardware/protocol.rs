//! Simple protocole handling module for `sans` and `monospace`
//!
//! The protocol is documented at length in the wiki [here]!
//!
//! [here]: https://github.com/libreflip/sans/wiki
//!
//! When making changes, make sure to test against the
//! mock connection which can be run via the
//! `hw::mock_serial` test-suite:
//!
//! ```console
//! $ cargo test hw::mock_serial --no-default-features
//! ```


/// A one-dimentional direction
pub enum Direction {
    Up = 1,
    Down = 0,
}

/// A command to send to the hardware
pub enum Command {

    /// Move the box either up or down
    MoveBox(Direction),

    /// Enable or disable lights
    Lighting(bool),

    /// Turn page, sending spine width
    FlipPage(u8),
}

impl Command {

    /// Encode a command into a byte array
    pub fn encode(self) -> Vec<u8> {
        use Command::*;
        match self {
            MoveBox(dir) => vec![0b00000010, dir as u8],
            Lighting(state) => vec![0b00000100, state as u8],
            FlipPage(spine) => vec![0b00001000, spine],
        }
    }
}

pub enum Status {
    Cool,
    Error,
}

impl From<u8> for Status {
    fn from(code: u8) -> Self {
        match code {
            0 => Status::Cool,
            _ => Status::Error,
        }
    }
}

/// Ab abstraction over responses sent by the hardware
///
/// If no payload was present `payload` is an empty vector
pub struct Response {
    status: Status,
    payload: Vec<u8>,
}

impl Response {
    /// Pass a lambda that returns a byte via reads
    pub fn build<F>(mut req_byte: F) -> Option<Self>
    where
        F: FnMut() -> Option<u8>,
    {
        let status = req_byte()?.into();
        let len = req_byte()?;

        let payload = (0..len).map(|_| req_byte()).fold(Some(Vec::new()), |mut vec, byte| {
            match (&mut vec, byte) {
                (Some(ref mut v), Some(b)) => v.push(b),
                _ => return None,
            };

            vec
        })?;

        Some(Self { status, payload })
    }


    // pub fn parse(data: Vec<u8>) -> Option<Self> {
    //     let s = data.get(0)?;
    //     let len = data.get(1)?;

    //     let payload = if *len != 0 {
    //         &data[2..2+(*len) as usize]
    //     } else {
    //         unimplemented!()
    //     };

    //     Some(Self {
    //         status: Status::Ok,
    //         payload: payload.into(),
    //     })
    // }
}
