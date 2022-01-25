# rtltcp

[![Rust GitHub Template](https://img.shields.io/badge/Rust%20GitHub-Template-blue)](https://rust-github.github.io/)
[![Crates.io](https://img.shields.io/crates/v/rtltcp.svg)](https://crates.io/crates/rtltcp)
[![Docs.rs](https://docs.rs/rtltcp/badge.svg)](https://docs.rs/rtltcp)
[![CI](https://github.com/niclashoyer/rtltcp/workflows/CI/badge.svg)](https://github.com/niclashoyer/rtltcp/actions)
[![Coverage Status](https://coveralls.io/repos/github/niclashoyer/rtltcp/badge.svg?branch=main)](https://coveralls.io/github/niclashoyer/rtltcp?branch=main)

A rust implementation of [rtl-tcp](https://github.com/pinkavaj/rtl-sdr/blob/master/src/rtl_tcp.c)
with better buffering and support for systemd [socket activation](http://0pointer.de/blog/projects/socket-activation.html).

## Installation

### Download the latest binary release

Download the [latest release](https://github.com/niclashoyer/rtltcp/releases) of rtltcp and place it in `/usr/local/bin`:

```bash
# ARMv7 (e.g. Raspberry Pi)
wget https://github.com/niclashoyer/rtltcp/releases/download/0.1.0/rtltcp-raspbian-armv7 -O /usr/local/bin/rtltcp
chmod +x /usr/local/bin/rtltcp
```

### Cargo

If you want to build the code using your own rust toolchain, you can use `cargo` to do this for you.

* Install the rust toolchain in order to have cargo installed by following
  [this](https://www.rust-lang.org/tools/install) guide.
* run `cargo install rtltcp`

### Using Systemd Socket Activation

By using systemd socket activation it is possible to start rtltcp just if there is a connection. This keeps the rtl-sdr stick cool while not in use without any effort on the server side.

To use socket activation, place a file `rtltcp.service` and a file `rtltcp.socket` in `/etc/systemd/system/`.

`rtltcp.service`:

```ini
[Unit]
Description=RTL TCP Service
After=network.target
Requires=rtltcp.socket

[Service]
Type=notify
User=pi
ExecStart=/usr/local/bin/rtltcp
TimeoutStopSec=5
```

`rtltcp.socket`:
```ini
[Unit]
Description=RTL TCP Socket
PartOf=rtltcp.service

[Socket]
ListenStream=[::]:1234

[Install]
WantedBy=sockets.target
```

Install rtltcp either by using `cargo install` or download the latest release (see above).
Now enable and start the socket:

```bash
systemctl enable rtltcp.socket
systemctl start rtltcp.socket
```

Systemd should now be listening on port 1234 and start/stop rtltcp automatically.

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

See [CONTRIBUTING.md](CONTRIBUTING.md).
