//! A common selection of pre-processing workloads done by sans

use process::Work;

pub struct ColorCorrect {

}

impl Work for ColorCorrect {
    fn add_workload(&mut self) {

    }

    fn work(&mut self) -> Result<i32, ()> {
        Ok(0)
    }
}