//! One reusable, sequential PhotoKit/Core Image/Vision worker. The observer has its own process.
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, sync_channel},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const STOPPED: &str = "Apple services stopped. Use Refresh Photos to reconnect; no operation was replayed.";

#[derive(Default)]
struct State {
    service: Option<Arc<Service>>,
    starts: u64,
    closed: bool,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(Mutex::default)
}

struct Process {
    child: Mutex<Option<Child>>,
    pid: u32,
    stopped: AtomicBool,
    active: AtomicBool,
    requests: AtomicU64,
}
impl Process {
    fn stop(&self, graceful: bool) {
        self.stopped.store(true, Ordering::Release);
        let mut guard = self.child.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(child) = guard.as_mut() {
            let started = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if graceful && started.elapsed() < Duration::from_millis(200) => std::thread::sleep(Duration::from_millis(5)),
                    _ => {
                        let _ = child.kill();
                        break;
                    }
                }
            }
            let _ = child.wait();
        }
        guard.take();
    }
}

struct Transport {
    input: Option<ChildStdin>,
    replies: Receiver<Result<Value, String>>,
    sequence: u64,
}
struct Service {
    process: Arc<Process>,
    transport: Mutex<Transport>,
    reader: Option<JoinHandle<()>>,
}

impl Service {
    fn launch(helper: &Path) -> Result<Self, String> {
        let mut child = Command::new(helper)
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting Apple service: {e}"))?;
        let Some((input, output)) = child.stdin.take().zip(child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Apple service pipes are unavailable".into());
        };
        let process = Arc::new(Process {
            pid: child.id(),
            child: Mutex::new(Some(child)),
            stopped: AtomicBool::new(false),
            active: AtomicBool::new(false),
            requests: AtomicU64::new(0),
        });
        let control = process.clone();
        let (tx, replies) = sync_channel(1);
        let reader = std::thread::Builder::new().name("lc-apple-service".into()).spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let reply = read_reply(&mut output);
                let failed = reply.is_err();
                // Only one request is outstanding. A full slot means unsolicited/duplicate output.
                if tx.try_send(reply).is_err() || failed {
                    if !control.stopped.load(Ordering::Acquire) {
                        control.stop(false);
                    }
                    break;
                }
            }
        });
        match reader {
            Ok(reader) => Ok(Self { process, transport: Mutex::new(Transport { input: Some(input), replies, sequence: 0 }), reader: Some(reader) }),
            Err(error) => {
                process.stop(false);
                Err(format!("starting Apple service reader: {error}"))
            }
        }
    }

    fn call(&self, data: &[u8], timeout: Duration) -> Result<Value, String> {
        if self.process.stopped.load(Ordering::Acquire) {
            return Err(STOPPED.into());
        }
        let mut io = self.transport.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.process.stopped.load(Ordering::Acquire) {
            return Err(STOPPED.into());
        }
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let input = directory.path().join("request.json");
        std::fs::write(&input, data).map_err(|e| e.to_string())?;
        io.sequence = io.sequence.checked_add(1).ok_or("Apple service sequence overflow")?;
        let sequence = io.sequence;
        let mut command = serde_json::to_vec(&json!({"id":sequence,"input":input})).map_err(|e| e.to_string())?;
        if command.len() > 2048 {
            return Err("Apple service request path is too long".into());
        }
        command.push(b'\n');
        self.process.active.store(true, Ordering::Release);
        self.process.requests.fetch_add(1, Ordering::Relaxed);
        let response = (|| -> Result<Value, String> {
            let input = io.input.as_mut().ok_or(STOPPED)?;
            input.write_all(&command).and_then(|()| input.flush()).map_err(|e| format!("writing Apple service request: {e}"))?;
            let reply = io.replies.recv_timeout(timeout).map_err(|e| format!("Apple service response: {e}; use Refresh Photos to reconnect"))??;
            if reply.get("id").and_then(Value::as_u64) != Some(sequence) {
                return Err("Apple service reply is out of order".into());
            }
            reply.get("result").cloned().ok_or_else(|| "Missing Apple service result".into())
        })();
        self.process.active.store(false, Ordering::Release);
        if response.is_err() {
            self.process.stop(false);
        }
        let result = response?;
        // A normal image/permission/decoder error must not restart a healthy helper.
        if let Some(error) = super::response_error(&result) {
            return Err(error);
        }
        Ok(result)
    }

    fn stop(&self) {
        self.process.stopped.store(true, Ordering::Release);
        let graceful = if let Ok(mut io) = self.transport.try_lock() {
            io.input.take();
            true
        } else {
            false
        };
        self.process.stop(graceful);
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_reply(output: &mut impl BufRead) -> Result<Value, String> {
    let mut data = Vec::new();
    output.take(192_001).read_until(b'\n', &mut data).map_err(|e| format!("reading Apple service: {e}"))?;
    if data.len() > 192_000 {
        return Err("Oversized Apple service response".into());
    }
    if data.is_empty() {
        return Err("Apple service disconnected. Use Refresh Photos to reconnect; no operation was replayed.".into());
    }
    serde_json::from_slice(&data).map_err(|e| format!("Invalid Apple service response: {e}"))
}

pub(super) fn invoke(request: &Value, timeout: Duration) -> Result<Value, String> {
    let data = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    if data.len() > 65_536 {
        return Err("oversized Apple bridge request".into());
    }
    let service = {
        let mut state = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(STOPPED.into());
        }
        if state.service.is_none() {
            state.service = Some(Arc::new(Service::launch(&super::native::helper()?)?));
            state.starts = state.starts.saturating_add(1);
        }
        state.service.clone().ok_or(STOPPED)?
    };
    service.call(&data, timeout)
}

pub(super) fn shutdown() {
    let service = {
        let mut state = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.service.take()
    };
    if let Some(service) = service {
        service.stop();
    }
}
pub(super) fn reset() {
    let service = {
        let mut state = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = false;
        if state.service.as_ref().is_some_and(|s| s.process.stopped.load(Ordering::Acquire)) { state.service.take() } else { None }
    };
    if let Some(service) = service {
        service.stop();
    }
}
pub(super) fn status() -> Value {
    let state = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(service) = &state.service else {
        return json!({"state":if state.closed { "closed" } else { "idle" },"starts":state.starts,"requests":0});
    };
    json!({"pid":service.process.pid,"starts":state.starts,"requests":service.process.requests.load(Ordering::Relaxed),
        "state":if service.process.stopped.load(Ordering::Acquire) { "stopped" } else if service.process.active.load(Ordering::Acquire) { "busy" } else { "ready" }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_requests_and_image_errors_reuse_one_process() {
        let service = Service::launch(&super::super::native::helper().unwrap()).unwrap();
        let pid = service.process.pid;
        for _ in 0..40 {
            assert_eq!(service.call(br#"{"operation":"health"}"#, Duration::from_secs(10)).unwrap()["protocol"], 2);
        }
        assert!(service.call(br#"{"operation":"invalid-test-operation"}"#, Duration::from_secs(10)).unwrap_err().contains("Unknown"));
        assert_eq!(service.call(br#"{"operation":"health"}"#, Duration::from_secs(10)).unwrap()["protocol"], 2);
        assert_eq!(service.process.pid, pid);
        assert_eq!(service.process.requests.load(Ordering::Relaxed), 42);
        drop(service);
        assert!(!Command::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "pid="]).output().unwrap().status.success());
    }

    #[test]
    fn timeouts_stop_and_reap_without_automatically_replaying() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join("stalled");
        std::fs::write(&helper, "#!/bin/sh\nread -r first\nread -r never\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::launch(&helper).unwrap();
        let pid = service.process.pid;
        assert!(service.call(br#"{"operation":"health"}"#, Duration::from_millis(40)).is_err());
        assert!(service.call(br#"{"operation":"health"}"#, Duration::from_millis(40)).unwrap_err().contains("stopped"));
        assert_eq!(service.process.requests.load(Ordering::Relaxed), 1);
        assert!(!Command::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "pid="]).output().unwrap().status.success());
    }

    #[test]
    fn invalid_and_oversized_replies_are_bounded() {
        assert!(read_reply(&mut &b""[..]).unwrap_err().contains("disconnected"));
        assert!(read_reply(&mut &b"nope\n"[..]).unwrap_err().contains("Invalid"));
        assert!(read_reply(&mut vec![b'x'; 192_001].as_slice()).unwrap_err().contains("Oversized"));
    }

    #[test]
    fn oversized_photos_adjustments_are_rejected_without_accessing_photos_or_crashing_the_worker() {
        let service = Service::launch(&super::super::native::helper().unwrap()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recipe.json");
        let recipe = json!({"schema":1,"originalAssetId":"synthetic-no-library-access","mask":"a".repeat(8_000_000)});
        std::fs::write(&path, serde_json::to_vec(&recipe).unwrap()).unwrap();
        let request = serde_json::to_vec(&json!({"operation":"photos-save-copy", "asset":"synthetic-no-library-access", "recipe":path})).unwrap();
        let error = service.call(&request, Duration::from_secs(10)).unwrap_err();
        assert!(error.contains("exceeds 64 KB"), "{error}");
        assert!(!service.process.stopped.load(Ordering::Acquire));
        assert_eq!(service.call(br#"{"operation":"health"}"#, Duration::from_secs(10)).unwrap()["protocol"], 2);
        assert_eq!(service.process.requests.load(Ordering::Relaxed), 2);
    }
}
