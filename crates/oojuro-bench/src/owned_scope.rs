//! Identity-checked ownership of one generated benchmark systemd scope.
//! Whole-cgroup termination is required for arms which fork, notably PostgreSQL.
use crate::{Result, sys};
use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) struct OwnedScope {
    unit: String,
    invocation: String,
    pub(crate) cgroup: PathBuf,
}

/// Only bounded-output systemctl control commands are used here. Keep the
/// command owned until wait, and kill/reap it on deadline; never by a guessed PID.
fn control(args: &[&str]) -> Result<String> {
    let mut child = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let signal = child.kill();
            let reap = child.wait();
            signal?;
            reap?;
            return Err("benchmark systemctl deadline exceeded".into());
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut out = String::new();
    child
        .stdout
        .take()
        .ok_or("systemctl stdout missing")?
        .take(4097)
        .read_to_string(&mut out)?;
    if out.len() > 4096 {
        return Err("benchmark systemctl output exceeds bound".into());
    }
    if !status.success() {
        return Err(format!("benchmark systemctl failed: {status}").into());
    }
    Ok(out)
}
fn fields(unit: &str) -> Result<(String, String)> {
    let text = control(&[
        "show",
        "--property=InvocationID",
        "--property=ControlGroup",
        unit,
    ])?;
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or("")
            .to_owned()
    };
    Ok((field("InvocationID="), field("ControlGroup=")))
}
impl OwnedScope {
    pub(crate) fn capture(unit: String, pid: u32) -> Result<Self> {
        let (invocation, path) = fields(&unit)?;
        if invocation.len() != 32
            || !invocation.bytes().all(|b| b.is_ascii_hexdigit())
            || !path.starts_with('/')
        {
            return Err("benchmark scope identity unavailable".into());
        }
        let cgroup = PathBuf::from(format!("/sys/fs/cgroup{path}"));
        if sys::cgroup_of(pid).as_ref() != Some(&cgroup) {
            return Err("benchmark leader is not in its owned scope".into());
        }
        Ok(Self {
            unit,
            invocation,
            cgroup,
        })
    }
    pub(crate) fn empty(&self) -> Result<bool> {
        match fs::read_to_string(self.cgroup.join("cgroup.events")) {
            Ok(text) => {
                let value = text
                    .lines()
                    .find_map(|line| line.strip_prefix("populated "))
                    .ok_or("missing cgroup population")?;
                match value {
                    "0" => Ok(true),
                    "1" => Ok(false),
                    _ => Err("invalid cgroup population".into()),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e.into()),
        }
    }
    pub(crate) fn kill_all(&self) -> Result<()> {
        // If already empty/collected there is nothing to signal. The generated
        // unit is never reused; refuse a different invocation before mutation.
        if self.empty()? {
            return Ok(());
        }
        let (invocation, path) = fields(&self.unit)?;
        if invocation != self.invocation
            || PathBuf::from(format!("/sys/fs/cgroup{path}")) != self.cgroup
        {
            return Err("benchmark scope changed; refuse stale termination".into());
        }
        control(&["kill", "--kill-whom=all", "--signal=SIGKILL", &self.unit])?;
        self.wait_empty(Duration::from_secs(5))
    }
    pub(crate) fn wait_empty(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.empty()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("owned benchmark cgroup remains populated".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
