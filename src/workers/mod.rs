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
    fn work(&self) -> Result<i32, ()>;
}

/*

What this module needs to expose

 - "Work" interface which can be used to implement LocalWorker and RemoteWorker
 - "Worker" interface which can be used to compute work locally or receive it via an API
 - "Scheduler" which is configured to either talk to a local worker or send the work to an external 
      worker to process.

  Worker (local)   |   Worker
     ^^^           |     ^^^
  Scheduler        |   [Use worker API]
     ^^^           |     ^^^
     WORK          |   Scheduler  
                   |     ^^^  
                   |     WORK
*/