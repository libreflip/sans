//! A simple scheduler for sans processing
//!
//! There is no scheduler trait (yet). This implementation can be configured with
//! the Type enum into either `local` or `remote` work mode, binding against either
//! implementation for [Worker].
//!

use api::{rx, tx};

pub struct Scheduler {
    _type: Type
}

pub enum Type {
    LOCAL,
    REMOTE,
}

impl Scheduler {
    pub fn new(_type: Type) {
        return Scheduler { _type: _type };
    }
}