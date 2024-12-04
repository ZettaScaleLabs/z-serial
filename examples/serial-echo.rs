//
// Copyright (c) 2022 ZettaScale Technology
//
// This program and the accompanying materials are made available under the
// terms of the Eclipse Public License 2.0 which is available at
// http://www.eclipse.org/legal/epl-2.0, or the Apache License, Version 2.0
// which is available at https://www.apache.org/licenses/LICENSE-2.0.
//
// SPDX-License-Identifier: EPL-2.0 OR Apache-2.0
//
// Contributors:
//   ZettaScale Zenoh Team, <zenoh@zettascale.tech>
//

use clap::Parser;
use std::{process::exit, time::Duration};

use z_serial::ZSerial;

static DEFAULT_INTERVAL: &str = "1.0";
static DEFAULT_RATE: &str = "9600";

#[derive(Parser, Debug)]
struct Args {
    port: String,
    #[clap(short, long)]
    server: bool,
    #[clap(short, long, default_value = DEFAULT_RATE)]
    baud_rate: u32,
    #[clap(short, long, default_value = DEFAULT_INTERVAL)]
    interval: f64,
}

#[tokio::main]
async fn main() -> tokio_serial::Result<()> {
    // initiate logging
    env_logger::init();

    let args = Args::parse();
    let mut buff = [0u8; 65535];

    println!("Arguments: {:?}", args);

    let mut port = ZSerial::new(args.port, args.baud_rate, false)?;

    if args.server {
        loop {
            port.accept().await?;

            'inner: loop {
                match port.read_msg(&mut buff).await {
                    Ok(read) => {
                        println!(">> Read {read} bytes: {:02X?}", &buff[0..read]);

                        port.write(&buff[..read]).await?;

                        println!("<< Echoed back");
                    }
                    Err(e) => {
                        println!("Got error: {e} received {:02X?}", &buff[..8]);
                        break 'inner;
                    }
                }
            }
        }
    } else {
        let mut count = 1usize;
        let mut lost = 0usize;

        let timeout_duration = if args.interval > 0.5 {
            3.0 * args.interval
        } else {
            2.0
        };

        port.connect(None).await?;

        loop {
            tokio::time::sleep(Duration::from_secs_f64(args.interval)).await;

            let data = count.to_ne_bytes();

            port.write(&data).await?;

            println!("<< Wrote {} bytes bytes: {:02X?}", data.len(), data);

            let timeout = async move {
                tokio::time::sleep(Duration::from_secs_f64(timeout_duration)).await;
            };

            tokio::select! {
                res = port.read_msg(&mut buff) => {
                    let read = res?;
                    if read > 0 {
                        println!(">> Read {read} bytes: {:02X?}", &buff[0..read]);
                        println!("Read: {}", usize::from_ne_bytes(buff[..read].try_into().expect("slice with incorrect length")));
                    }
                    count = count.wrapping_add(1);
                },
                _ = timeout => {
                    count = count.wrapping_add(1);
                    lost = lost.wrapping_add(1);
                },
                _ = tokio::signal::ctrl_c() => {
                    println!("Sent a total of {count} messages, lost {lost}");
                    exit(0);
                }
            };
        }
    }
}
