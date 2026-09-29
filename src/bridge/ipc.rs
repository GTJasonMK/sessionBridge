use super::{store::Store, *};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{Arc, Condvar, Mutex, mpsc},
    time::Duration,
};

pub type SharedStore = Arc<Mutex<Store>>;
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub struct Hub {
    // Lifecycle changes are infrequent; serialize them while waiting for owned processes to exit.
    pub(crate) lifecycle: Mutex<()>,
    pub stores: Mutex<HashMap<String, SharedStore>>,
    pub running: Mutex<HashMap<String, mpsc::Sender<()>>>,
    revision: Mutex<u64>,
    changed: Condvar,
    operator: String,
    shutdown: AtomicBool,
}
impl Hub {
    pub fn notify(&self) {
        let mut revision = match self.revision.lock() {
            Ok(revision) => revision,
            Err(poisoned) => poisoned.into_inner(),
        };
        *revision += 1;
        self.changed.notify_all();
    }
    pub fn store(&self, root: &str) -> Result<SharedStore> {
        let canonical = Path::new(root).canonicalize()?;
        let root = canonical
            .to_str()
            .ok_or_else(|| error("INVALID_INPUT", "workspace path must be UTF-8"))?
            .to_owned();
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| error("INTERNAL", "store registry poisoned"))?;
        if let Some(store) = stores.get(&root) {
            return Ok(store.clone());
        }
        let store = Store::open(Path::new(&root))?;
        process::recover(&store)?;
        store.db.execute("UPDATE members SET state='offline',token_hash=NULL,error='服务重启，请恢复并核实未完成操作' WHERE role!='boss' AND state IN ('starting','busy','ready')",[])?;
        let store = Arc::new(Mutex::new(store));
        stores.insert(root, store.clone());
        Ok(store)
    }
    pub fn is_stopping(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
    pub fn request(self: &Arc<Self>, request: Request) -> Result<Value> {
        if request.operation == "member.initialize" {
            let shared = self.store(&request.workspace)?;
            let actor = {
                let store = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?;
                store.authorize(&request.token, &self.operator, "member.initialize")?
            };
            if actor != "boss" {
                return Err(error("FORBIDDEN", "boss required"));
            }
            let result = runtime::initialize(
                self.clone(),
                shared,
                text(&request.args, "member")?,
                &request.args,
                &actor,
            )?;
            self.notify();
            return Ok(result);
        }
        let lifecycle = matches!(
            request.operation.as_str(),
            "service.shutdown"
                | "member.stop"
                | "member.configure"
                | "member.remove"
                | "room.transition"
                | "room.integrate"
        );
        let _control = if lifecycle {
            Some(
                self.lifecycle
                    .lock()
                    .map_err(|_| error("INTERNAL", "lifecycle lock poisoned"))?,
            )
        } else {
            None
        };
        if request.operation == "watch" {
            if request.token != self.operator {
                return Err(error("UNAUTHORIZED", "operator credential required"));
            }
            let after = optional_integer(&request.args, "after", 0)?;
            let revision = self
                .revision
                .lock()
                .map_err(|_| error("INTERNAL", "revision poisoned"))?;
            let (revision, _) = self
                .changed
                .wait_timeout_while(revision, Duration::from_secs(25), |r| {
                    !self.is_stopping() && *r == after as u64
                })
                .map_err(|_| error("INTERNAL", "watch poisoned"))?;
            return Ok(json!({"revision":*revision,"stopping":self.is_stopping()}));
        }
        if self.is_stopping() {
            return Err(error("SHUTTING_DOWN", "service is shutting down"));
        }
        if request.operation == "service.shutdown" {
            if request.token != self.operator {
                return Err(error("UNAUTHORIZED", "operator credential required"));
            }
            self.shutdown.store(true, Ordering::SeqCst);
            self.notify();
            let stores: Vec<_> = self
                .stores
                .lock()
                .map_err(|_| error("INTERNAL", "registry poisoned"))?
                .values()
                .cloned()
                .collect();
            for shared in stores {
                let members = {
                    let store = shared
                        .lock()
                        .map_err(|_| error("INTERNAL", "store poisoned"))?;
                    store.transaction(|| {
                        let rooms = store.rows("SELECT id FROM rooms WHERE state='active'", &[])?;
                        store
                            .db
                            .execute("UPDATE rooms SET state='paused' WHERE state='active'", [])?;
                        for room in rooms {
                            store.system(text(&room, "id")?, "后台停止，工作群已暂停")?;
                        }
                        Ok(())
                    })?;
                    store.rows(
                        "SELECT id FROM members WHERE role!='boss' AND state!='removed'",
                        &[],
                    )?
                };
                for member in members {
                    runtime::stop(self, &shared, text(&member, "id")?)?;
                }
            }
            return Ok(json!({"stopped":true}));
        }
        if request.operation == "providers" {
            if request.token != self.operator {
                return Err(error("UNAUTHORIZED", "operator credential required"));
            }
            return provider::list();
        }
        // Member credentials never create or recover an arbitrary workspace before authentication.
        let shared = if request.token == self.operator {
            self.store(&request.workspace)?
        } else {
            let root = Path::new(&request.workspace).canonicalize()?;
            self.stores
                .lock()
                .map_err(|_| error("INTERNAL", "registry poisoned"))?
                .get(
                    root.to_str()
                        .ok_or_else(|| error("INVALID_INPUT", "workspace path must be UTF-8"))?,
                )
                .cloned()
                .ok_or_else(|| error("UNAUTHORIZED", "workspace not opened by the boss"))?
        };
        let boss_lifecycle = lifecycle && request.operation != "room.integrate";
        if boss_lifecycle && request.token != self.operator {
            return Err(error("FORBIDDEN", "boss required"));
        }
        let result = match request.operation.as_str() {
            "member.stop" => runtime::stop(self, &shared, text(&request.args, "member")?),
            "room.transition" => {
                let room = text(&request.args, "room")?;
                let next_state = text(&request.args, "state")?;
                let store = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?;
                let result = store.execute("boss", "room.transition", &request.args)?;
                let members = store.rows(
                    "SELECT id FROM members WHERE room_id=? AND role!='boss' AND state!='removed'",
                    &[&room],
                )?;
                drop(store);
                if next_state != "active" {
                    for member in members {
                        runtime::stop(self, &shared, text(&member, "id")?)?;
                    }
                }
                Ok(result)
            }
            "room.integrate" => {
                let store = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?;
                let actor = store.authorize(&request.token, &self.operator, "room.integrate")?;
                let result = git::integrate(&store, &actor, &request.args)?;
                let room = text(&request.args, "room")?;
                let members = store.rows(
                    "SELECT id FROM members WHERE room_id=? AND role!='boss' AND state!='removed'",
                    &[&room],
                )?;
                drop(store);
                for member in members {
                    runtime::stop(self, &shared, text(&member, "id")?)?;
                }
                Ok(result)
            }
            "member.configure" | "member.remove" => {
                let member = text(&request.args, "member")?;
                let running = self
                    .running
                    .lock()
                    .map_err(|_| error("INTERNAL", "runtime registry poisoned"))?
                    .contains_key(member);
                if running {
                    runtime::stop(self, &shared, member)?;
                }
                let store = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?;
                store.execute("boss", &request.operation, &request.args)
            }
            "check.run" => {
                let root = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?
                    .root
                    .clone();
                let store = Store::open(&root)?;
                let actor = store.authorize(&request.token, &self.operator, "check.run")?;
                git::check(
                    &store,
                    &actor,
                    &request.args,
                    if actor == "boss" {
                        None
                    } else {
                        Some(&request.token)
                    },
                )
            }
            op => {
                let store = shared
                    .lock()
                    .map_err(|_| error("INTERNAL", "store poisoned"))?;
                let actor = store.authorize(&request.token, &self.operator, op)?;
                let result = store.execute(&actor, op, &request.args);
                if result.as_ref().is_err_and(|e| e.code == "CHAIN_LIMIT") && actor != "boss" {
                    store.transaction(|| {
                        let m = store.member(&actor)?;
                        store
                            .db
                            .execute("UPDATE members SET token_hash=NULL WHERE id=?", [&actor])?;
                        store.system(
                            text(&m, "room_id")?,
                            "自动交流达到上限，正在停止当前成员；请检查后恢复。",
                        )?;
                        Ok(())
                    })?;
                    if let Some(sender) = self
                        .running
                        .lock()
                        .map_err(|_| error("INTERNAL", "registry poisoned"))?
                        .get(&actor)
                    {
                        sender
                            .send(())
                            .map_err(|_| error("RUNTIME", "worker has exited without cleanup"))?;
                    }
                }
                result
            }
        };
        if !matches!(
            request.operation.as_str(),
            "history" | "room.get" | "room.list" | "workspace.get"
        ) {
            self.notify();
        }
        result
    }
}
pub fn operator_token() -> Result<String> {
    Ok(std::fs::read_to_string(
        state_dir()?.join("operator.token"),
    )?)
}
pub fn socket() -> Result<PathBuf> {
    Ok(state_dir()?.join("bridge.sock"))
}
pub fn read_frame(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut line = String::new();
    let n = reader
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if n > MAX_FRAME_BYTES || !line.ends_with('\n') {
        return Err(error(
            "FRAME_TOO_LARGE",
            "protocol frame exceeds 2 MiB or is incomplete",
        ));
    }
    Ok(Some(serde_json::from_str(&line)?))
}
pub fn write_frame(writer: &mut impl Write, value: &Value) -> Result<()> {
    let mut frame = serde_json::to_vec(value)?;
    frame.push(b'\n');
    if frame.len() > MAX_FRAME_BYTES {
        return Err(error("FRAME_TOO_LARGE", "protocol frame exceeds 2 MiB"));
    }
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}
pub fn call(request: &Request) -> Result<Value> {
    let mut stream = UnixStream::connect(socket()?)?;
    stream.set_read_timeout(Some(Duration::from_secs(75)))?;
    write_frame(&mut stream, &serde_json::to_value(request)?)?;
    let response = read_frame(&mut BufReader::new(stream))?
        .ok_or_else(|| error("IPC", "service closed connection"))?;
    if response["ok"] == true {
        Ok(response["data"].clone())
    } else {
        Err(serde_json::from_value(response["error"].clone())?)
    }
}
pub fn start(binary: &Path) -> Result<()> {
    if available()? {
        return Ok(());
    }
    let directory = state_dir()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("service.log"))?;
    std::process::Command::new(binary)
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()?;
    for _ in 0..100 {
        if available()? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(error(
        "SERVICE_START",
        "service did not start; inspect service.log",
    ))
}
fn available() -> Result<bool> {
    match UnixStream::connect(socket()?) {
        Ok(_) => Ok(true),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}
pub fn serve() -> Result<()> {
    let directory = state_dir()?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("service.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(error("ALREADY_RUNNING", "service already running"));
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    let operator = match std::fs::read_to_string(directory.join("operator.token")) {
        Ok(token) => token,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let token = secret()?;
            private_write(&directory.join("operator.token"), token.as_bytes())?;
            token
        }
        Err(e) => return Err(e.into()),
    };
    let socket = socket()?;
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let hub = Arc::new(Hub {
        lifecycle: Mutex::new(()),
        stores: Mutex::new(HashMap::new()),
        running: Mutex::new(HashMap::new()),
        revision: Mutex::new(now() as u64),
        changed: Condvar::new(),
        operator,
        shutdown: AtomicBool::new(false),
    });
    let workspaces = directory.join("workspaces");
    if workspaces.exists() {
        for entry in std::fs::read_dir(workspaces)? {
            let path = entry?.path().join("workspace.json");
            if path.exists() {
                let workspace: Value = serde_json::from_slice(&std::fs::read(path)?)?;
                if let Err(e) = hub.store(text(&workspace, "root")?) {
                    eprintln!("restore workspace: {e}");
                }
            }
        }
    }
    let scheduler = hub.clone();
    std::thread::spawn(move || {
        while !scheduler.is_stopping() {
            if scheduler.shutdown.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = runtime::schedule(&scheduler) {
                eprintln!("scheduler: {e}");
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        std::thread::sleep(Duration::from_millis(100));
    });
    let mut workers = Vec::new();
    while !hub.shutdown.load(Ordering::SeqCst) {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let hub = hub.clone();
        workers.push(std::thread::spawn(move || {
            let result = (|| {
                stream.set_read_timeout(Some(Duration::from_secs(10)))?;
                let frame = read_frame(&mut BufReader::new(stream.try_clone()?))?;
                match frame {
                    Some(frame) => hub.request(serde_json::from_value(frame)?),
                    None => Ok(Value::Null),
                }
            })();
            let response = match result {
                Ok(v) => json!({"ok":true,"data":v}),
                Err(e) => json!({"ok":false,"error":e}),
            };
            if let Err(e) = write_frame(&mut stream, &response) {
                if e.code == "FRAME_TOO_LARGE" {
                    if let Err(send_error) =
                        write_frame(&mut stream, &json!({"ok":false,"error":e}))
                    {
                        eprintln!("IPC response: {send_error}");
                    }
                } else {
                    eprintln!("IPC response: {e}");
                }
            }
        }));
        let mut active = Vec::new();
        for worker in workers {
            if worker.is_finished() {
                worker
                    .join()
                    .map_err(|_| error("IPC", "request worker panicked"))?;
            } else {
                active.push(worker);
            }
        }
        workers = active;
    }
    for worker in workers {
        worker
            .join()
            .map_err(|_| error("IPC", "request worker panicked"))?;
    }
    std::fs::remove_file(socket)?;
    Ok(())
}
