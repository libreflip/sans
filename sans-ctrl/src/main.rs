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
        .subcommand(SubCommand::with_name("status").about("Get the server daemon status"))
        .subcommand(SubCommand::with_name("jobs").about("Get information about available jobs"))
        .subcommand(
            SubCommand::with_name("do")
                .about("Issue direct commands to the remote server")
                .subcommand(
                    SubCommand::with_name("restart").about("Restart the remote server stack"),
                )
                .subcommand(SubCommand::with_name("stop").about(
                    "Shutdown the remote server (warning, can't restart remotely after stop)",
                )),
        )
        .get_matches();

    // println!("Hello, world!");
}
