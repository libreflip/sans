//! (General) processing submodule for sans
//!
//! This module can be run in two different states. `local` and `remote`
//!
//! If in `remote` mode, all request calls will be routed to the external (remote) worker server
//! whereas in `local` mode sans will do all computations locally.
//!
//! This module also handles export tasks via an additional trait hook called [Export] which can be
//! hooked into a variety of web services or external API's.
//!
//! ## Design principles
//!
//!  - "Work" interface which can be used to implement LocalWorker and RemoteWorker
//!  - "Worker" interface which can be used to compute work locally or receive it via an API
//!  - "Scheduler" which is configured to either talk to a local worker
//!    or send the work to an external worker to process.
//!
//! ```
//!   Worker (local)   |   Worker
//!     ^^^           |     ^^^
//!   Scheduler        |   [Use worker API]
//!     ^^^           |     ^^^
//!     WORK          |   Scheduler
//!                   |     ^^^
//!                   |     WORK
//! ```
//!


pub mod scheduling;
pub mod pre;
pub mod post;


/// A trait that describes a piece of work to be done
///
/// It can be locally initialised, then sent to a remote work server, returning
/// a `Result` type for the processing that was done.
pub trait Work {
    fn add_workload(&mut self);
    fn work(&mut self) -> Result<i32, ()>;
}


/// A trait that describes a work handler
///
/// Any struct implementing this trait will have to accept any object that implements
/// the [Work] trait. Work can only be dispatched asynchronously as no garuantee can be
/// given about the order that work is executed in. It is however possible to fence the
/// result of a work ID – avoid doing this too much, not knowing if the worker is implemented
/// on a remote machine, causing high network load.
///
pub trait Worker {
    fn dispatch(&self, id: &str, work: Work);
    fn fence(id: &str) -> bool;
}


/// A trait that enables exporting to different platform targets.
///
/// This is kept as generic as possible and will be called on the right Work-types
/// after being processed.
pub trait Export {
    fn export(&self);
}
