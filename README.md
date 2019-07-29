
A rust implementation of [rtl-tcp](https://github.com/pinkavaj/rtl-sdr/blob/master/src/rtl_tcp.c)
with better buffering and support for systemd [socket activation](http://0pointer.de/blog/projects/socket-activation.html).

### Using Systemd Socket Activation on the Raspberry Pi

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

Download the latest release of rtltcp for the Raspberry Pi and place it in `/usr/local/bin`:

```bash
wget -O /usr/local/bin/rtltcp
chmod +x /usr/local/bin/rtltcp
```

Now enable and start the socket:

```bash
systemctl enable rtltcp.socket
systemctl start rtltcp.socket
```

The Raspberry Pi should now be listening on port 1234 and start/stop rtltcp automatically.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
