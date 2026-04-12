//! The kernel daemon — listens on a Unix domain socket and serves
//! agent sessions to connected distro processes.

mod router;

use kernel_interfaces::framing::{read_message, write_message};
use kernel_interfaces::protocol::{KernelEvent, KernelRequest};
use router::ConnectionRouter;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse simple args
    let socket_path = args
        .iter()
        .position(|a| a == "--socket")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/agent-kernel-{}.sock", std::process::id())));

    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "claude-sonnet-4-20250514".into());

    let api_key = std::env::var("ANTHROPIC_API_KEY").ok();

    // Clean up stale socket
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    let listener = UnixListener::bind(&socket_path).unwrap_or_else(|e| {
        eprintln!("failed to bind {}: {e}", socket_path.display());
        std::process::exit(1);
    });

    eprintln!("kernel daemon listening on {}", socket_path.display());
    if api_key.is_some() {
        eprintln!("using model: {model}");
    } else {
        eprintln!("no ANTHROPIC_API_KEY — using echo provider");
    }

    // Accept connections in a loop — when a distro disconnects,
    // go back to waiting for the next one.
    loop {
        eprintln!("waiting for connection...");

        let (stream, _addr) = match listener.accept() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };

        eprintln!("distro connected");

        let reader_stream = stream.try_clone().expect("clone stream for reader");
        let writer_stream = stream;

        // Channel for outgoing events
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<KernelEvent>();

        // Writer thread: sends KernelEvents to the distro
        let writer_handle = std::thread::spawn(move || {
            let mut writer = BufWriter::new(writer_stream);
            for event in event_rx {
                if write_message(&mut writer, &event).is_err() {
                    break;
                }
            }
        });

        // Router handles all protocol logic
        let mut router = ConnectionRouter::new(event_tx, api_key.clone(), model.clone());

        // Reader loop: reads KernelRequests from the distro
        let mut reader = BufReader::new(reader_stream);
        let mut shutdown = false;
        loop {
            let request: KernelRequest = match read_message(&mut reader) {
                Ok(req) => req,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        eprintln!("distro disconnected");
                    } else {
                        eprintln!("read error: {e}");
                    }
                    break;
                }
            };

            if !router.handle_request(request) {
                shutdown = true;
                break;
            }
        }

        let _ = writer_handle.join();

        if shutdown {
            break;
        }

        eprintln!("ready for next connection");
    }

    // Clean up
    let _ = std::fs::remove_file(&socket_path);
    eprintln!("kernel daemon stopped");
}
