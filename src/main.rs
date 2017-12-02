//! Sans – Libreflip backend daemon


mod binding;
mod process;
// use process::scheduling::scheduler::{Scheduler, Type};

mod rest;
mod state;
mod utilities;

fn main() {
    // let s = Scheduler::new(Type::LOCAL);
}


//     let counter = Arc::new(Mutex::new(0));
//     let mut handles = vec![];

//     for _ in 0..10 {
//         let counter = Arc::clone(&counter);
//         let handle = thread::spawn(move || {
//             let mut num = counter.lock().unwrap();
//             *num += 1;
//         });
//         handles.push(handle);
//     }

//     for handle in handles {
//         handle.join().unwrap();
//     }

//     println!("Result: {}", *counter.lock().unwrap());
// }