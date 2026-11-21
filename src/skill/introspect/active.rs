use crate::skill::exit::{data, Code, Error, Result};
use std::{
    ffi::OsStr,
    io::{self, Read},
    os::unix::{io::AsFd, process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
pub struct Capture {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
fn nonblock(fd: &impl AsFd) -> io::Result<()> {
    use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
    Ok(fcntl_setfl(fd, fcntl_getfl(fd)? | OFlags::NONBLOCK)?)
}
fn drain(reader: &mut impl Read, out: &mut Vec<u8>) -> Result<bool> {
    let mut buf = [0; 8192];
    for _ in 0..16 {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                if out.len() + n > 4 * 1024 * 1024 {
                    return Err(data("introspection output exceeds 4 MiB"));
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(false)
}
fn collect(child: &mut Child, ms: u64) -> Result<Capture> {
    let start = Instant::now();
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    nonblock(&out)?;
    nonblock(&err)?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    loop {
        if start.elapsed() >= Duration::from_millis(ms) {
            return Err(Error(
                Code::Timeout,
                format!("introspection exceeded {ms} ms"),
            ));
        }
        let a = drain(&mut out, &mut stdout)?;
        let b = drain(&mut err, &mut stderr)?;
        if a && b {
            if let Some(status) = child.try_wait()? {
                return Ok(Capture {
                    code: status.code(),
                    stdout,
                    stderr,
                });
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
}
pub fn capture(program: &Path, args: &[impl AsRef<OsStr>], ms: u64) -> Result<Capture> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs([
            ("CI", "1"),
            ("TERM", "dumb"),
            ("NO_COLOR", "1"),
            ("LC_ALL", "C"),
        ])
        .process_group(0)
        .spawn()?;
    let result = collect(&mut child, ms);
    if result.is_err() {
        use rustix::process::{kill_process_group, Pid, Signal};
        if let Some(pid) = Pid::from_raw(child.id() as i32) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        let _ = child.wait();
    }
    result
}
pub fn help(program: &Path, path: &[String], ms: u64) -> Result<String> {
    for flag in ["--help", "-h"] {
        let args: Vec<_> = path.iter().map(String::as_str).chain([flag]).collect();
        let result = capture(program, &args, ms)?;
        if matches!(result.code, Some(0 | 2)) {
            let s = format!(
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            if super::parser::signature(&s) {
                return Ok(s);
            }
        }
    }
    Err(data(format!(
        "unrecognized help for {} {}",
        program.display(),
        path.join(" ")
    )))
}
