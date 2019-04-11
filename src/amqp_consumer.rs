use std::net::SocketAddr;
use std::thread;

use lapin_futures::channel::{BasicConsumeOptions, QueueDeclareOptions};
use lapin_futures::client::ConnectionOptions;
use lapin_futures::types::FieldTable;

use failure::Error;
use lapin_futures as lapin;
use log::{debug, info};
use tokio;
use tokio::net::TcpStream;
use tokio::prelude::*;
use tokio::runtime::current_thread::block_on_all;

use serde_json;


#[derive(Debug, Deserialize, Clone, Serialize)]
enum Command {
    SftpDownload { path: String },
    HttpDownload { url: String }
}

pub struct AmqpListener {
    pub addr: String,
}

impl AmqpListener {
    pub fn start_consumer(self) -> thread::JoinHandle<()> {
        thread::spawn(move || -> () {
            let addr: SocketAddr = self.addr.parse().unwrap();

            block_on_all(
                TcpStream::connect(&addr).map_err(Error::from).and_then(|stream| {

                    // connect() returns a future of an AMQP Client
                    // that resolves once the handshake is done
                    lapin::client::Client::connect(stream, ConnectionOptions::default()).map_err(Error::from)
                }).and_then(|(client, heartbeat)| {
                    // The heartbeat future should be run in a dedicated thread so that nothing can prevent it from
                    // dispatching events on time.
                    // If we ran it as part of the "main" chain of futures, we might end up not sending
                    // some heartbeats if we don't poll often enough (because of some blocking task or such).
                    tokio::spawn(heartbeat.map_err(|_| ()));

                    // create_channel returns a future that is resolved
                    // once the channel is successfully created
                    client.create_channel().map_err(Error::from)
                }).and_then(|channel| {
                    let id = channel.id;
                    info!("created channel with id: {}", id);

                    let ch = channel.clone();
                    let queue_name = "text";

                    channel.queue_declare(queue_name, QueueDeclareOptions::default(), FieldTable::new()).and_then(move |queue| {
                        info!("channel {} declared queue {}", id, queue_name);

                        // basic_consume returns a future of a message
                        // stream. Any time a message arrives for this consumer,
                        // the for_each method would be called
                        channel.basic_consume(&queue, "my_consumer", BasicConsumeOptions::default(), FieldTable::new())
                    }).and_then(|stream| {
                        info!("got consumer stream");

                        stream.for_each(move |message| {
                            debug!("got message: {:?}", message);
                            let decoded_message = std::str::from_utf8(&message.data).unwrap();
                            info!("decoded message: {}", decoded_message);

                            let deserialize_result: serde_json::Result<Command> = serde_json::from_slice(message.data.as_slice());

                            match deserialize_result {
                                Ok(command) => info!("{:?}", command),
                                Err(e) => error!("Error deserializing command: {}", e)
                            }

                            ch.basic_ack(message.delivery_tag, false)
                        })
                    }).map_err(Error::from)
                })
            ).expect("runtime failure");
        })
    }
}
