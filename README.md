# Zenoh Serial

This repo contains some code for testing serial framing on rust

# Build it

You need `rust` and `Cargo`, look on the rust website how to install it.

```bash
$ cd ~
$ git clone https://github.com/ZettaScaleLabs/z-serial
$ cd z-serial
$ cargo build --release
```

# How to test it

Open two terminals, on the first one start the echo server:

```bash
$ cd ~/z-serial
$ ./target/release/examples/serial-echo <serial device> -s -b <baud rate>
...
```

On the second one start the client
```bash
$ cd ~/z-serial
./target/release/examples/serial-echo <serial device>  -i <send interval> -b <baud rate>
```