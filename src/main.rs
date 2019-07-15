
use std::convert::TryInto;
use std::io::prelude::*;
use std::io::BufWriter;
use std::net::TcpListener;

#[cfg(feature = "systemd")]
use listenfd::ListenFd;
use slog::{o, debug, error, info};
use slog::Drain;

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let decorator = slog_term::TermDecorator::new().build();
	let drain = slog_term::FullFormat::new(decorator).build().fuse();
	let drain = slog_async::Async::new(drain).build().fuse();
	let log = slog::Logger::root(drain, o!());

	let listener;
	#[cfg(feature = "systemd")]
	{
		let mut listenfd = ListenFd::from_env();
		listener = if let Some(listener) = listenfd.take_tcp_listener(0).map_err(|_| "Could not get file descriptor from input")? {
			listener
		} else {
			TcpListener::bind("[::]:1234")?
		};
		systemd::daemon::notify(false, [(systemd::daemon::STATE_READY, "1")].iter())?;
	}
	#[cfg(not(feature = "systemd"))]
	{
		listener = TcpListener::bind("[::]:1234")?;
	}

	for stream in listener.incoming() {
		let stream = stream?;
		let (mut ctl, mut reader) = rtlsdr_mt::open(0).map_err(|_| "Could not open RTL SDR device")?;
		ctl.enable_agc().map_err(|_| "Could not enable automatic gain control")?;
		ctl.set_ppm(0).map_err(|_| "Could not set PPM to zero")?;
		ctl.set_center_freq(100_000_000).map_err(|_| "Could not set center frequency")?;

		std::thread::spawn({
			let log = log.clone();
			let mut stream = stream.try_clone()?;
			move || {
				let mut buf = [0; 5];
				loop {
					stream.read(&mut buf).unwrap();
					match buf[0] {
						0x01 => {
							let freq = u32::from_be_bytes((&buf[1..5]).try_into().unwrap());
							info!(log, "setting center freq to {}", freq);
							ctl.set_center_freq(freq).unwrap();
						},
						0x02 => {
							let sample_rate = u32::from_be_bytes((&buf[1..5]).try_into().unwrap());
							info!(log, "setting sample rate to {}", sample_rate);
							ctl.set_sample_rate(sample_rate).unwrap();
						},
						0x05 => {
							let ppm = i32::from_be_bytes((&buf[1..5]).try_into().unwrap());
							info!(log, "setting ppm to {}", ppm);
							ctl.set_ppm(ppm).unwrap();
						},
						0x04 => {
							let gain = i32::from_be_bytes((&buf[1..5]).try_into().unwrap());
							info!(log, "setting manual gain to {}", gain);
							ctl.set_tuner_gain(gain).unwrap();
						},
						0x08 => {
							let agc = u32::from_be_bytes((&buf[1..5]).try_into().unwrap()) == 1u32;
							if agc {
								info!(log, "setting automatic gain control to on");
								ctl.enable_agc().unwrap();
							} else {
								info!(log, "setting automatic gain control to off");
								ctl.disable_agc().unwrap();
							}
						},
						_ => {
							debug!(log, "recv unsupported command {:?}", buf);
						}
					}
					//let next = ctl.center_freq() + 1000;
					//ctl.set_center_freq(next).unwrap();
				}
			}
		});

		let mut buf_write_stream = BufWriter::with_capacity(500*1024, stream);
		let mut magic_packet = vec![];
		magic_packet.extend_from_slice(b"RTL0");
		magic_packet.extend_from_slice(&5u32.to_be_bytes()); // FIXME
		magic_packet.extend_from_slice(&[0x00, 0x00, 0x00, 0x1d]); // FIXME
		buf_write_stream.write(&magic_packet)?;
		reader.read_async(15, 0, |bytes| {
			buf_write_stream.write(&bytes).unwrap_or_else(|err| {
				error!(log, "error while writing to TCP stream: {:?}", err); // FIXME: async logger does not flush
				std::process::exit(1);
			});
		}).unwrap();
	}

	Ok(())

}
