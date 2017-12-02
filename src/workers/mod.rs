//! Image worker submodule
//!
//! This module can be run in two different states. `local` and `remote`
//!
//! If in `remote` mode, all request calls will be routed to the external (remote) worker server
//! whereas in `local` mode sans will do all computations locally.


/// A trait that describes a piece of work to be done
///
/// It can be locally initialised, then sent to a remote work server, returning
/// a `Result` type for the processing that was done.
pub trait Work {
    fn add_workload(&self);
    fn work(&self) -> Result<()>;
}

pub struct Workers {
    
}
