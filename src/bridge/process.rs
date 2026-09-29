//! Owned process groups: stop children too, and never signal a reused PID.
use super::*;
use std::process::{Child, Command};
use std::time::Duration;

pub fn configure(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let parent = std::process::id();
    // SAFETY: prctl is an async-signal-safe syscall; no allocation or locks occur after fork.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent as libc::pid_t {
                return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
}
pub fn fingerprint(pid: u32) -> std::io::Result<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or_else(|| std::io::Error::other("invalid process stat"))?
        .1;
    let start = fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| std::io::Error::other("process start time unavailable"))?;
    Ok(format!(
        "{}:{start}",
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim()
    ))
}
fn signal(pid: u32, signal: i32) -> Result<()> {
    let group = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| error("PROCESS", "invalid process group"))?;
    // SAFETY: kill only addresses the validated, owned process group.
    if unsafe { libc::kill(-group, signal) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e.into());
        }
    }
    Ok(())
}
pub fn terminate(child: &mut Child) -> Result<()> {
    let pid = child.id();
    signal(pid, libc::SIGTERM)?;
    // Keep the leader unreaped so the group ID cannot be reused before all children are killed.
    std::thread::sleep(Duration::from_millis(100));
    signal(pid, libc::SIGKILL)?;
    child.wait()?;
    Ok(())
}
pub fn recover(store: &store::Store) -> Result<()> {
    for row in store.rows("SELECT * FROM processes", &[])? {
        let pid = row["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 1)
            .ok_or_else(|| error("PROCESS", "invalid process ID"))?;
        match fingerprint(pid) {
            Ok(actual) if actual == text(&row, "fingerprint")? => {
                signal(pid, libc::SIGTERM)?;
                std::thread::sleep(Duration::from_millis(100));
                // Kill the originally identified group while its ID is still reserved, if alive.
                match fingerprint(pid) {
                    Ok(current) if current == actual => signal(pid, libc::SIGKILL)?,
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(_) => {} // The old process exited; this PID now belongs to someone else.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    store.db.execute("DELETE FROM processes", [])?;
    Ok(())
}
