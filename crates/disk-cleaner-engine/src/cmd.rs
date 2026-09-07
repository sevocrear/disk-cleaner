use crate::config::Config;
use std::process::{Command, Output};
use std::time::Duration;

pub struct CmdResult {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn which_bin(name: &str) -> Option<String> {
    which::which(name)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

pub fn run_command(cmd: &[String], cfg: &Config) -> CmdResult {
    if cmd.is_empty() {
        return CmdResult {
            status: 1,
            stdout: String::new(),
            stderr: "empty command".into(),
        };
    }
    let mut timeout = Duration::from_secs(cfg.cmd_timeout_sec);
    if cmd[0] == "docker" && timeout.as_secs() < cfg.docker_timeout_sec {
        if cmd.iter().any(|x| x == "prune" || x == "rmi" || x == "builder") {
            timeout = Duration::from_secs(cfg.docker_timeout_sec);
        }
    }

    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]);
    // Best-effort timeout via wait_timeout isn't in std; use spawn + wait with thread.
    match run_with_timeout(c, timeout) {
        Ok(output) => CmdResult {
            status: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(e) => CmdResult {
            status: 124,
            stdout: String::new(),
            stderr: format!("{e}\n[disk-cleaner] timed out after {}s", timeout.as_secs()),
        },
    }
}

fn run_with_timeout(mut c: Command, timeout: Duration) -> Result<Output, String> {
    use std::sync::mpsc;
    use std::thread;

    let child = c
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid_hint = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            // Kill process group best-effort
            let _ = Command::new("kill")
                .args(["-9", &pid_hint.to_string()])
                .status();
            Err(format!("timeout after {}s", timeout.as_secs()))
        }
    }
}
