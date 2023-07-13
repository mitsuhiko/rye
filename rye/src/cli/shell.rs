use std::env;
use std::path::PathBuf;
use std::process;
use std::process::Command;

use anyhow::{bail, Context, Error};
use clap::Parser;
use console::style;
use sysinfo::{Pid, PidExt, ProcessExt, System, SystemExt};

use crate::pyproject::PyProject;
use crate::sync::{sync, SyncOptions};
use crate::tui::redirect_to_stderr;
use crate::utils::QuietExit;

/// Spawns a shell with the virtualenv activated.
#[derive(Parser, Debug)]
pub struct Args {
    /// Do not show banner
    #[arg(long)]
    no_banner: bool,
    /// Allow nested invocations.
    #[arg(long)]
    allow_nested: bool,
    /// Use this pyproject.toml file
    #[arg(long, value_name = "PYPROJECT_TOML")]
    pyproject: Option<PathBuf>,
}

fn get_shell() -> Result<String, Error> {
    let shell_env = env::var("SHELL");
    if let Ok(shell) = shell_env {
        return Ok(shell);
    }

    let mut system = System::default();
    system.refresh_processes();

    let mut pid = Some(Pid::from_u32(process::id()));
    while let Some(p) = pid {
        if let Some(process) = system.process(p) {
            match process.name() {
                "cmd.exe" => {
                    return Ok(String::from("cmd.exe"));
                }
                "powershell.exe" => {
                    return Ok(String::from("powershell.exe"));
                }
                "pwsh.exe" => {
                    return Ok(String::from("pwsh.exe"));
                }
                &_ => {
                    pid = process.parent();
                    continue;
                }
            }
        }
        break;
    }

    Err(anyhow::anyhow!("don't know which shell is used"))
}

fn is_ms_shells(shell: &str) -> bool {
    matches!(shell, "cmd.exe" | "powershell.exe" | "pwsh.exe")
}

pub fn execute(cmd: Args) -> Result<(), Error> {
    if !cmd.allow_nested && env::var("__RYE_SHELL").ok().as_deref() == Some("1") {
        bail!("cannot invoke recursive rye shell");
    }

    let _guard = redirect_to_stderr(true);
    let pyproject = PyProject::load_or_discover(cmd.pyproject.as_deref())?;
    sync(SyncOptions::python_only().pyproject(cmd.pyproject))
        .context("failed to sync ahead of shell")?;

    let venv_path = pyproject.venv_path();
    let venv_bin = if env::consts::OS == "windows" {
        venv_path.join("Scripts")
    } else {
        venv_path.join("bin")
    };

    let s = get_shell()?;
    let sep = if is_ms_shells(s.as_str()) { ";" } else { ":" };
    let args = if !is_ms_shells(s.as_str()) {
        vec!["-l"]
    } else {
        vec![]
    };
    let mut shell = Command::new(s.as_str());
    shell.args(args).env("VIRTUAL_ENV", &*venv_path);

    if let Some(path) = env::var_os("PATH") {
        let mut new_path = venv_bin.as_os_str().to_owned();
        new_path.push(sep);
        new_path.push(path);
        shell.env("PATH", new_path);
    } else {
        shell.env("PATH", &*venv_bin);
    }
    shell.env_remove("PYTHONHOME");
    shell.env("__RYE_SHELL", "1");

    if !cmd.no_banner {
        echo!(
            "Spawning virtualenv shell from {}",
            style(&venv_path.display()).cyan()
        );
        echo!("Leave shell with 'exit'");
    }

    let status = shell.status()?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        Err(QuietExit(code).into())
    } else {
        Ok(())
    }
}
