use std::io::prelude::*;
use std::io::BufWriter;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

use clap::Parser;
#[cfg(feature = "systemd")]
use listenfd::ListenFd;
use tracing::{debug, info, warn};

/// RTL-TCP protocol command codes
const COMMAND_HEADER_SIZE: usize = 5;
const CMD_SET_FREQUENCY: u8 = 0x01;
const CMD_SET_SAMPLE_RATE: u8 = 0x02;
const CMD_SET_GAIN_MODE: u8 = 0x03;
const CMD_SET_TUNER_GAIN: u8 = 0x04;
const CMD_SET_PPM: u8 = 0x05;
const CMD_SET_AGC: u8 = 0x08;

/// Magic packet sent to client on connect:
/// "RTL0" (4 bytes) + tuner type 5 (4 bytes BE) + max gain value 0x1d (4 bytes BE)
const MAGIC_PACKET: &[u8] = b"RTL0\x00\x00\x00\x05\x00\x00\x00\x1d";

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about = "an I/Q spectrum server for RTL2832 based DVB-T receivers",
    long_about = None
)]
struct Args {
    /// listen address
    #[clap(short, long, default_value = "[::]")]
    address: String,

    /// listen port
    #[clap(short, long, default_value_t = 1234)]
    port: u16,

    /// device index
    #[clap(short, long, default_value_t = 0)]
    device_index: u32,

    /// number of decoding buffers
    #[clap(short, long, default_value_t = 15)]
    buffers: u32,

    /// tcp sending buffer size (in bytes)
    #[clap(short, long, default_value_t = 512000)]
    tcp_buffers: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let listener;
    #[cfg(feature = "systemd")]
    {
        let mut listenfd = ListenFd::from_env();
        listener = if let Some(listener) = listenfd
            .take_tcp_listener(0)
            .map_err(|_| "Could not get file descriptor from input")?
        {
            listener
        } else {
            TcpListener::bind(format!("{}:{}", args.address, args.port))?
        };
        systemd::daemon::notify(false, [(systemd::daemon::STATE_READY, "1")].iter())?;
    }
    #[cfg(not(feature = "systemd"))]
    {
        listener = TcpListener::bind(format!("{}:{}", args.address, args.port))?;
    }

    let (sender, receiver) = sync_channel(1);
    let sender_ctrlc = sender.clone();
    let should_exit = Arc::new(AtomicBool::new(false));
    let should_exit_ctrlc = should_exit.clone();
    ctrlc::set_handler(move || {
        info!("received signal, shutting down");
        match sender_ctrlc.try_send(()) {
            Ok(_) => {}
            Err(_) => {
                warn!("could not send exit signal, exiting immediately");
                should_exit_ctrlc.store(true, Ordering::SeqCst);
            }
        }
    })?;

    info!("waiting for connection…");
    let (stream, _addr) = listener.accept()?;
    let (ctl, mut reader) =
        rtlsdr_mt::open(args.device_index).map_err(|_| "Could not open RTL SDR device")?;
    let ctl = Arc::new(Mutex::new(ctl));

    let thread_ctl = std::thread::spawn({
        let ctl = ctl.clone();
        let should_exit = should_exit.clone();
        let mut stream = stream.try_clone()?;
        move || {
            let mut buf = [0u8; COMMAND_HEADER_SIZE];
            loop {
                match stream.read_exact(&mut buf) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof
                        || e.kind() == ErrorKind::ConnectionReset
                        || e.kind() == ErrorKind::BrokenPipe
                        || e.kind() == ErrorKind::TimedOut =>
                    {
                        info!("client disconnected: {}", e);
                        break;
                    }
                    Err(e) => {
                        warn!("read error from client: {}", e);
                        break;
                    }
                }

                if should_exit.load(Ordering::SeqCst) {
                    info!("exit flag set, stopping control thread");
                    break;
                }

                let cmd = buf[0];
                let payload: [u8; 4] = [buf[1], buf[2], buf[3], buf[4]];

                match cmd {
                    CMD_SET_FREQUENCY => {
                        let freq = u32::from_be_bytes(payload);
                        info!("setting center freq to {}", freq);
                        match ctl.lock() {
                            Ok(mut guard) => {
                                if let Err(_) = guard.set_center_freq(freq) {
                                    warn!("failed to set center freq");
                                }
                            }
                            Err(_) => {
                                warn!("mutex poisoned in control thread");
                                break;
                            }
                        }
                    }
                    CMD_SET_SAMPLE_RATE => {
                        let sample_rate = u32::from_be_bytes(payload);
                        info!("setting sample rate to {}", sample_rate);
                        match ctl.lock() {
                            Ok(mut guard) => {
                                if let Err(_) = guard.set_sample_rate(sample_rate) {
                                    warn!("failed to set sample rate");
                                }
                            }
                            Err(_) => {
                                warn!("mutex poisoned in control thread");
                                break;
                            }
                        }
                    }
                    CMD_SET_GAIN_MODE => {
                        let gain_mode = i32::from_be_bytes(payload);
                        if gain_mode > 0 {
                            info!("gain mode set to manual (AGC off)");
                            match ctl.lock() {
                                Ok(mut guard) => {
                                    if let Err(_) = guard.disable_agc() {
                                        warn!("failed to disable AGC");
                                    }
                                }
                                Err(_) => {
                                    warn!("mutex poisoned in control thread");
                                    break;
                                }
                            }
                        } else {
                            info!("gain mode set to automatic (AGC on)");
                            match ctl.lock() {
                                Ok(mut guard) => {
                                    if let Err(_) = guard.enable_agc() {
                                        warn!("failed to enable AGC");
                                    }
                                }
                                Err(_) => {
                                    warn!("mutex poisoned in control thread");
                                    break;
                                }
                            }
                        }
                    }
                    CMD_SET_TUNER_GAIN => {
                        let gain = i32::from_be_bytes(payload);
                        info!("setting manual gain to {}", gain);
                        match ctl.lock() {
                            Ok(mut guard) => {
                                if let Err(_) = guard.set_tuner_gain(gain) {
                                    warn!("failed to set tuner gain");
                                }
                            }
                            Err(_) => {
                                warn!("mutex poisoned in control thread");
                                break;
                            }
                        }
                    }
                    CMD_SET_PPM => {
                        let ppm = i32::from_be_bytes(payload);
                        info!("setting ppm to {}", ppm);
                        match ctl.lock() {
                            Ok(mut guard) => {
                                if let Err(_) = guard.set_ppm(ppm) {
                                    warn!("failed to set ppm");
                                }
                            }
                            Err(_) => {
                                warn!("mutex poisoned in control thread");
                                break;
                            }
                        }
                    }
                    CMD_SET_AGC => {
                        let agc = u32::from_be_bytes(payload) == 1u32;
                        if agc {
                            info!("setting automatic gain control to on");
                            match ctl.lock() {
                                Ok(mut guard) => {
                                    if let Err(_) = guard.enable_agc() {
                                        warn!("failed to enable AGC");
                                    }
                                }
                                Err(_) => {
                                    warn!("mutex poisoned in control thread");
                                    break;
                                }
                            }
                        } else {
                            info!("setting automatic gain control to off");
                            match ctl.lock() {
                                Ok(mut guard) => {
                                    if let Err(_) = guard.disable_agc() {
                                        warn!("failed to disable AGC");
                                    }
                                }
                                Err(_) => {
                                    warn!("mutex poisoned in control thread");
                                    break;
                                }
                            }
                        }
                    }
                    _ => {
                        debug!("recv unsupported command {:?}", buf);
                    }
                }
            }
            info!("control thread exiting");
        }
    });

    let thread_cancel = std::thread::spawn({
        let ctl = ctl.clone();
        let should_exit = should_exit.clone();
        move || {
            let _ = receiver.recv();
            info!("stopping read from device");
            if let Ok(mut guard) = ctl.lock() {
                guard.cancel_async_read();
            }
            should_exit.store(true, Ordering::SeqCst);
        }
    });

    let mut buf_write_stream = BufWriter::with_capacity(args.tcp_buffers, stream);
    buf_write_stream.write_all(MAGIC_PACKET)?;

    let read_result = reader.read_async(args.buffers, 0, |bytes| {
        if let Err(_err) = buf_write_stream.write_all(bytes) {
            let _ = sender.try_send(());
        }
    });

    // Signal cancel thread so it doesn't hang on recv() if read_async completes normally
    let _ = sender.try_send(());

    if let Err(_) = read_result {
        warn!("read_async error");
    }

    // Flush buffer before shutting down
    if let Err(e) = buf_write_stream.flush() {
        warn!("failed to flush write buffer: {}", e);
    }

    if let Err(e) = thread_cancel.join() {
        warn!("cancel thread panicked: {:?}", e);
    }
    if let Err(e) = thread_ctl.join() {
        warn!("control thread panicked: {:?}", e);
    }

    info!("rtltcp shut down successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_magic_packet_length() {
        assert_eq!(MAGIC_PACKET.len(), 12);
    }

    #[test]
    fn test_command_constants() {
        assert_eq!(CMD_SET_FREQUENCY, 0x01);
        assert_eq!(CMD_SET_SAMPLE_RATE, 0x02);
        assert_eq!(CMD_SET_GAIN_MODE, 0x03);
        assert_eq!(CMD_SET_TUNER_GAIN, 0x04);
        assert_eq!(CMD_SET_PPM, 0x05);
        assert_eq!(CMD_SET_AGC, 0x08);
    }

    #[test]
    fn test_command_header_size() {
        assert_eq!(COMMAND_HEADER_SIZE, 5);
    }

    #[test]
    fn test_magic_packet_content() {
        // "RTL0" + 5 (tuner type) + 0x1d (max gain)
        assert_eq!(&MAGIC_PACKET[0..4], b"RTL0");
        assert_eq!(&MAGIC_PACKET[4..8], &5u32.to_be_bytes());
        assert_eq!(&MAGIC_PACKET[8..12], &0x1du32.to_be_bytes());
    }
}
