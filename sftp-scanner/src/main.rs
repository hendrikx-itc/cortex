use std::thread;
use std::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::Write;

use env_logger;
use futures::stream::Stream;
use futures::Future;
use log::{debug, error, info};
use tokio;
use tokio_executor::enter;

use crossbeam_channel::bounded;

use signal_hook;
use signal_hook::iterator::Signals;

extern crate config;

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate prometheus;

#[macro_use]
extern crate lazy_static;

extern crate chrono;
extern crate postgres;
extern crate serde_yaml;

extern crate cortex_core;

mod cmd;
mod http_server;
mod metrics;
mod settings;
mod sftp_scanner;
mod amqp_sender;

use settings::Settings;

fn main() {
    let matches = cmd::app().get_matches();

    let mut env_logger_builder = env_logger::builder();

    // When run as a service no timestamps are logged, we expect the service manager to append
    // timestamps to the logs.
    if matches.is_present("service") {
        env_logger_builder.format(|buf, record| {
            writeln!(buf, "{}  {}", record.level(), record.args())
        });
    }

    env_logger_builder.init();

    if matches.is_present("sample_config") {
        print!(
            "{}\n",
            serde_yaml::to_string(&settings::Settings::default()).unwrap()
        );
        ::std::process::exit(0);
    }

    let config_file = matches
        .value_of("config")
        .unwrap_or("/etc/cortex/sftp-scanner.yaml");

    let settings = load_settings(&config_file);

    let mut entered = enter().expect("Failed to claim thread");
    let mut runtime = tokio::runtime::Runtime::new().unwrap();

    // Will hold all functions that stop components of the SFTP scannner
    let mut stop_commands: Vec<Box<dyn FnOnce() -> () + Send + 'static>> = Vec::new();

    // Setup the channel that connects to the RabbitMQ queue for SFTP download commands.
    let (cmd_sender, cmd_receiver) = bounded(4096);

    let stop = Arc::new(AtomicBool::new(false));

    let stop_clone = stop.clone();

    stop_commands.push(Box::new(move || {
        stop_clone.swap(true, Ordering::Relaxed);
    }));

    // Start every configured scanner in it's own thread and have them send commands to the
    // command channel.
    let scanner_threads: Vec<(String, thread::JoinHandle<()>)> = settings
        .sftp_sources
        .clone()
        .into_iter()
        .map(|sftp_source| {
            let name = sftp_source.name.clone();

            let join_handle = sftp_scanner::start_scanner(
                stop.clone(),
                cmd_sender.clone(),
                settings.postgresql.url.clone(),
                sftp_source,
            );

            (name, join_handle)
        })
        .collect();

    let metrics_collector_join_handle = match settings.prometheus_push {
        Some(conf) => {
            let join_handle = start_metrics_collector(conf.gateway.clone(), conf.interval);

            info!("Metrics collector thread started");

            Some(join_handle)
        }
        None => Option::None,
    };

    // Start the built in web server that currently only serves metrics.
    let (web_server_join_handle, actix_system, actix_http_server) = http_server::start_http_server(settings.http_server.address);

    stop_commands.push(Box::new(move || {
        tokio::spawn(actix_http_server.stop(true));
    }));

    stop_commands.push(Box::new(move || {
        actix_system.stop();
    }));

    let amqp_sender_join_handle = amqp_sender::start_sender(stop, cmd_receiver, settings.command_queue.address);

    // Use a stream to connect the command channel to the AMQP queue.
    //let future = channel_to_amqp(stop_receiver, cmd_receiver, &settings.command_queue.address);

    runtime.spawn(setup_signal_handler(stop_commands));

    //runtime.spawn(future);

    entered
        .block_on(runtime.shutdown_on_idle())
        .expect("Shutdown cannot error");

    for (source_name, scanner_thread) in scanner_threads {
        info!("Waiting for scanner thread '{}' to stop", &source_name);

        let res = scanner_thread.join();

        match res {
            Ok(()) => {
                info!("Scanner thread '{}' stopped", &source_name);
            }
            Err(e) => {
                error!("Scanner thread '{}' stopped with error: {:?}", &source_name, e)
            }
        }
    }

    let res = web_server_join_handle.join();

    match res {
        Ok(()) => info!("Http server thread stopped"),
        Err(e) => error!("Http server thread stopped with error: {:?}", e),
    }

    let res = amqp_sender_join_handle.join();

    match res {
        Ok(()) => info!("AMQP sender thread stopped"),
        Err(e) => error!("AMQP sender thread stopped with error: {:?}", e),
    }

    if let Some(join_handle) = metrics_collector_join_handle {
        let res = join_handle.join();

        match res {
            Ok(()) => info!("Metrics collector thread stopped"),
            Err(e) => error!("Metrics collector thread stopped with error: {:?}", e),
        }
    }
}

fn setup_signal_handler(stop_commands: Vec<Box<dyn FnOnce() -> () + Send + 'static>>) -> impl Future<Item = (), Error = ()> + Send + 'static {
    let signals = Signals::new(&[
        signal_hook::SIGHUP,
        signal_hook::SIGTERM,
        signal_hook::SIGINT,
        signal_hook::SIGQUIT,
    ]).unwrap();

    let signal_stream = signals.into_async().unwrap().into_future();

    signal_stream
        .map(move |sig| {
            info!("signal: {}", sig.0.unwrap());

            for stop_command in stop_commands {
                stop_command();
            }
        })
        .map_err(|e| panic!("{}", e.0))
}


fn load_settings(config_file: &str) -> Settings {
    info!("Loading configuration from file {}", config_file);

    let mut settings = config::Config::new();

    let merge_result = settings.merge(config::File::new(config_file, config::FileFormat::Yaml));

    match merge_result {
        Ok(_config) => {
            info!("Configuration loaded from file {}", config_file);
        }
        Err(e) => {
            error!("Error loading configuration: {}", e);
            ::std::process::exit(1);
        }
    }

    let into_result = settings.try_into();

    let settings: Settings = match into_result {
        Ok(s) => s,
        Err(e) => {
            error!("Error loading configuration: {}", e);
            ::std::process::exit(1);
        }
    };

    info!("Configuration loaded");

    settings
}

fn start_metrics_collector(address: String, push_interval: u64) -> thread::JoinHandle<()> {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(push_interval));

        let metric_families = prometheus::gather();
        let push_result = prometheus::push_metrics(
            "cortex-sftp-scanner",
            labels! {},
            &address,
            metric_families,
            Some(prometheus::BasicAuthentication {
                username: "user".to_owned(),
                password: "pass".to_owned(),
            }),
        );

        match push_result {
            Ok(_) => {
                debug!("Pushed metrics to Prometheus Gateway");
            }
            Err(e) => {
                error!("Error pushing metrics to Prometheus Gateway: {}", e);
            }
        }
    })
}
