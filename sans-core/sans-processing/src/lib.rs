//! Post processing working and scheduling module.
//! 
//! This crate is part of the libreflip project, you can find out
//! more about it on [libreflip-website]
//! 
//! [libreflip-website]: https://libreflip.org
//! 
//! sans-processing provides an easy to understand and use pipeline 
//! of two stages.
//! 
//! - Pre-processing filters
//! - Post-processing filters
//! 
//! When an image is taken it's run through a series of pre-processing filters
//! immediately. After a task (a complete set of pictures) has been captured, they
//! are then sequentially run through all post-processing filters.
//! 
//! sans-processing is heavily reliant on imagemagick 7.0 for it's computation
//! and as such you have to have this verison installed.
//! 
//! ## Writing your own filters
//! 
//! sans-processing is designed to be easily extended via the [filters] API.

/* Export public API for plugins */
pub mod filters;

/* Include internal filters */
mod pre;
mod post;


/// A work scheduler struct that is responsible for executing
/// work for many filters on as many cores as it has access to.
pub struct Worker {

}


/// A trait that describes a piece of work to be done
///
/// It can be locally initialised, then sent to a remote work server, returning
/// a `Result` type for the processing that was done.
pub trait Job {
    fn add_data(&mut self);
    fn run(&mut self) -> Result<i32, ()>;
}

