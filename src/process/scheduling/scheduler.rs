//! A simple scheduler for sans processing
//!
//! There is no scheduler trait (yet). This implementation can be configured with
//! the Type enum into either `local` or `remote` work mode, binding against either
//! implementation for [Worker].
//!

use process::Work;

pub struct Scheduler {
    t: Type
}

pub enum Type {
    LOCAL,
    REMOTE,
}

impl Scheduler {
    pub fn new(t: Type) -> Scheduler {
        return Scheduler { t: t };
    }

    pub fn dispatch(&mut self, work: Box<Work>) {
        
    }
}