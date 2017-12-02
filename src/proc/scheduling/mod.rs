//! Scheduling subsystem for sans processing
//! 
//! This module handles scheduler types and states, network transparency and easy
//! to use API's for dispatching and fencing work that is being done by a sans
//! daemon (somewhere 😝)
//! 
//! ```rust
//! use sans::proc::scheduler::{Scheduler, Type};
//! let s = Scheduler::new(Type::LOCAL);
//! s.dispatch(...);
//! ```

pub use scheduler;
pub use net_worker;
pub use loc_worker;