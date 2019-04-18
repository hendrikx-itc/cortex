use clap::{crate_name, crate_authors, crate_description, crate_version, App, Arg};

pub fn app() -> App<'static, 'static> {
    App::new(crate_name!())
        .version(crate_version!())
        .about(crate_description!())
        .author(crate_authors!())
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .value_name("CONFIG_FILE")
                .help("Specify config file")
                .takes_value(true),
        )
}