extern crate clap;

use clap::{App, AppSettings, Arg, SubCommand};

fn main() {
    let _m = App::new("sansctl")
        .setting(AppSettings::SubcommandRequired)
        .arg(
            Arg::with_name("target")
                .short("t")
                .long("target")
                .help("Specify the instance of sans to communicate with"),
        )
        .subcommand(SubCommand::with_name("status").help("Get the server daemon status"))
        .get_matches();

    // println!("Hello, world!");
}
