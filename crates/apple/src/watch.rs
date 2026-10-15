//! One PhotoKit subscription per desktop session. No periodic requests or resource reads.
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub enum PhotosEvent {
    Snapshot(Value),
    Changes(Value),
}

/// Dropping the subscription closes, kills and reaps the helper and joins its reader.
/// The receiver must be disconnected first if `deliver` can block on a bounded channel.
pub struct PhotosWatch {
    child: Child,
    input: Arc<Mutex<Option<ChildStdin>>>,
    reader: Option<JoinHandle<()>>,
}

impl PhotosWatch {
    pub fn start(deliver: impl Fn(Result<PhotosEvent, String>) -> bool + Send + 'static) -> Result<Self, String> {
        Self::launch(&super::native::helper()?, deliver)
    }

    fn launch(helper: &Path, deliver: impl Fn(Result<PhotosEvent, String>) -> bool + Send + 'static) -> Result<Self, String> {
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let output = directory.path().join("projection.json");
        let mut child = Command::new(helper)
            .arg("--photos-watch")
            .arg(&output)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting Photos observer: {e}"))?;
        let pipes = child.stdin.take().zip(child.stdout.take());
        let Some((input, stdout)) = pipes else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Photos observer pipes are unavailable".into());
        };
        let input = Arc::new(Mutex::new(Some(input)));
        let writer = input.clone();
        let reader = std::thread::Builder::new().name("lc-photos-observer".into()).spawn(move || {
            let _directory = directory;
            let mut stream = BufReader::new(stdout);
            let mut sequence = 0_u64;
            let result = (|| -> Result<(), String> {
                loop {
                    let header = read_header(&mut stream)?;
                    let next = sequence.checked_add(1).ok_or("Photos observer sequence overflow")?;
                    if header.get("sequence").and_then(Value::as_u64) != Some(next) {
                        return Err("Photos observer sequence is out of order; use Refresh Photos".into());
                    }
                    let event = header.get("event").and_then(Value::as_str).ok_or("Missing Photos observer event")?;
                    if !matches!((sequence, event), (0, "snapshot") | (1.., "changes")) {
                        return Err("Unexpected Photos observer event; use Refresh Photos".into());
                    }
                    let value = serde_json::from_slice(&super::native::bounded_read(&output, 128_000_000)?).map_err(|e| e.to_string())?;
                    let event = if event == "snapshot" { PhotosEvent::Snapshot(value) } else { PhotosEvent::Changes(value) };
                    if !deliver(Ok(event)) {
                        return Ok(());
                    }
                    sequence = next;
                    let mut input = writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let input = input.as_mut().ok_or("Photos observer closed")?;
                    input.write_all(b"next\n").and_then(|()| input.flush()).map_err(|e| format!("acknowledging Photos change: {e}"))?;
                }
            })();
            if let Err(error) = result {
                deliver(Err(error));
            }
        });
        match reader {
            Ok(reader) => Ok(Self { child, input, reader: Some(reader) }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(format!("starting Photos observer reader: {error}"))
            }
        }
    }
}

fn read_header(stream: &mut impl BufRead) -> Result<Value, String> {
    let mut line = Vec::new();
    stream.take(192_001).read_until(b'\n', &mut line).map_err(|e| format!("reading Photos observer: {e}"))?;
    if line.len() > 192_000 {
        return Err("Oversized Photos observer header".into());
    }
    if line.is_empty() {
        return Err("Photos observer stopped; use Refresh Photos to reconnect".into());
    }
    let header = serde_json::from_slice(&line).map_err(|e| format!("Invalid Photos observer header: {e}"))?;
    if let Some(error) = super::response_error(&header) {
        return Err(error);
    }
    Ok(header)
}

impl Drop for PhotosWatch {
    fn drop(&mut self) {
        // EOF lets PhotoKit unregister cleanly. A stalled startup still has a bounded stop.
        self.input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < Duration::from_millis(200) => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = self.child.kill();
                    break;
                }
            }
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_headers_are_bounded_and_report_native_errors() {
        assert!(read_header(&mut &b""[..]).unwrap_err().contains("stopped"));
        assert!(read_header(&mut &b"nope\n"[..]).unwrap_err().contains("Invalid"));
        assert!(read_header(&mut vec![b'x'; 192_001].as_slice()).unwrap_err().contains("Oversized"));
        assert_eq!(read_header(&mut &b"{\"error\":\"Photos access denied\"}\n"[..]).unwrap_err(), "Photos access denied");
        assert_eq!(read_header(&mut &b"{\"event\":\"snapshot\",\"sequence\":1}\n"[..]).unwrap()["sequence"], 1);
    }

    #[test]
    fn dropping_an_idle_subscription_reaps_the_helper() {
        use std::{os::unix::fs::PermissionsExt, sync::mpsc, time::Duration};
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("observer");
        std::fs::write(&script, "#!/bin/sh\nprintf '{\"assets\":[],\"albums\":[],\"authorization\":\"authorized\"}' > \"$2\"\nprintf '{\"event\":\"snapshot\",\"sequence\":1}\\n'\nread -r ack\nread -r stop\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (tx, rx) = mpsc::channel();
        let watch = PhotosWatch::launch(&script, move |event| tx.send(event).is_ok()).unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap(), PhotosEvent::Snapshot(_)));
        let pid = watch.child.id();
        drop(rx);
        drop(watch);
        assert!(!Command::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "pid="]).output().unwrap().status.success());
    }
}
