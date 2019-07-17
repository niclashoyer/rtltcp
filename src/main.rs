
use std::sync::Arc;
use std::convert::TryInto;
use std::io::prelude::*;
use std::io::BufWriter;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "systemd")]
use listenfd::ListenFd;
use slog::{o, debug, info};
use slog::Drain;
use clap::{App, Arg};

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let running = Arc::new(AtomicBool::new(false));
	let r = running.clone();
	ctrlc::set_handler(move || {
		if r.load(Ordering::SeqCst) == false {
			// rtl tcp not running yet, just exit
			std::process::exit(0);
		}
		r.store(false, Ordering::SeqCst);
	})?;

	let decorator = slog_term::TermDecorator::new().build();
	let drain = slog_term::FullFormat::new(decorator).build().fuse();
	let drain = slog_async::Async::new(drain).build().fuse();
	let log = slog::Logger::root(drain, o!());

	let matches = App::new("rtltcp")
		.version("0.1")
		.about("an I/Q spectrum server for RTL2832 based DVB-T receivers")
		.author("Niclas Hoyer")
		.arg(Arg::with_name("address")
			 .short("a")
			 .value_name("ADDRESS")
			 .help("listen address (default: [::])"))
		.arg(Arg::with_name("port")
			 .short("p")
			 .value_name("PORT")
			 .help("listen port (default: 1234)"))
		.arg(Arg::with_name("device")
			 .short("d")
			 .value_name("DEVICE_INDEX")
			 .help("device index (default: 0)"))
		.arg(Arg::with_name("buffers")
			 .short("b")
			 .value_name("NUM")
			 .help("number of decoding buffers (default: 15)"))
		.arg(Arg::with_name("tcp_buffer")
			 .short("n")
			 .value_name("BYTES")
			 .help("tcp sending buffer size (default: 500 KiB)"))
		.arg(Arg::with_name("ppm")
			 .short("P")
			 .value_name("NUM")
			 .help("ppm error correction value (default: 0)"))
		.arg(Arg::with_name("frequency")
			 .short("f")
			 .value_name("HZ")
			 .help("frequency to tune to (default: 100 MHz)"))
		.arg(Arg::with_name("gain")
			 .short("g")
			 .value_name("NUM")
			 .help("gain (default: 0 for auto)"))
		.arg(Arg::with_name("sample_rate")
			 .short("s")
			 .value_name("HZ")
			 .help("samplerate in Hz (default: 2048000 Hz)"))
		.get_matches();

	let addr = matches.value_of("address").unwrap_or("[::]");
	let port = matches.value_of("port").unwrap_or("1234");
	let device = matches.value_of("device").unwrap_or("0").parse::<u32>()?;
	let buffers = matches.value_of("buffers").unwrap_or("15").parse::<u32>()?;
	let tcpbufsize = matches.value_of("tcp_buffer").unwrap_or("512000").parse::<usize>()?;
	let ppm = matches.value_of("ppm").unwrap_or("0").parse::<i32>()?;
	let freq = matches.value_of("frequency").unwrap_or("100000000").parse::<u32>()?;
	let gain = matches.value_of("gain").unwrap_or("0").parse::<i32>()?;
	let sample_rate = matches.value_of("sample_rate").unwrap_or("2048000").parse::<u32>()?;

	let listener;
	#[cfg(feature = "systemd")]
	{
		let mut listenfd = ListenFd::from_env();
		listener = if let Some(listener) = listenfd.take_tcp_listener(0).map_err(|_| "Could not get file descriptor from input")? {
			listener
		} else {
			TcpListener::bind(format!("{}:{}", addr, port))?
		};
		systemd::daemon::notify(false, [(systemd::daemon::STATE_READY, "1")].iter())?;
	}
	#[cfg(not(feature = "systemd"))]
	{
		listener = TcpListener::bind(format!("{}:{}", addr, port))?;
	}

	for stream in listener.incoming() {
		running.store(true, Ordering::SeqCst);
		let stream = stream?;
		let (mut ctl, mut reader) = rtlsdr_mt::open(device).map_err(|_| "Could not open RTL SDR device")?;
		ctl.set_sample_rate(sample_rate).map_err(|_| "Could not set sample rate")?;
		if gain == 0 {
			ctl.enable_agc().map_err(|_| "Could not enable automatic gain control")?;
		} else {
			ctl.set_tuner_gain(gain).map_err(|_| "Could not enable manual gain")?;
		}
		ctl.set_ppm(ppm).map_err(|_| "Could not set PPM")?;
		ctl.set_center_freq(freq).map_err(|_| "Could not set center frequency")?;

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
				}
			}
		});

		let mut buf_write_stream = BufWriter::with_capacity(tcpbufsize, stream);
		let mut magic_packet = vec![];
		magic_packet.extend_from_slice(b"RTL0");
		magic_packet.extend_from_slice(&5u32.to_be_bytes()); // FIXME
		magic_packet.extend_from_slice(&[0x00, 0x00, 0x00, 0x1d]); // FIXME
		buf_write_stream.write(&magic_packet)?;
		reader.read_async(buffers, 0, |bytes| {
			if running.load(Ordering::SeqCst) == false {
				std::process::exit(0);
			}
			buf_write_stream.write(&bytes).unwrap_or_else(|_err| {
				std::process::exit(0);
			});
		}).unwrap();
	}

	Ok(())

}
