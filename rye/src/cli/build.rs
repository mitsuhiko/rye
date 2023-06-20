use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Error};
use clap::Parser;
use console::style;

use crate::bootstrap::ensure_self_venv;
use crate::pyproject::{normalize_package_name, PyProject};
use crate::utils::{get_venv_python_bin, CommandOutput};

/// Builds a package for distribution.
#[derive(Parser, Debug)]
pub struct Args {
    /// Build an sdist
    #[arg(long)]
    sdist: bool,
    /// Build a wheel
    #[arg(long)]
    wheel: bool,
    /// Build all packages
    #[arg(short, long)]
    all: bool,
    /// Build a specific package
    #[arg(short, long)]
    package: Vec<String>,
    /// An output directory (defaults to `workspace/dist`)
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Use this pyproject.toml file
    #[arg(long, value_name = "PYPROJECT_TOML")]
    pyproject: Option<PathBuf>,
    /// Clean the output directory first
    #[arg(short, long)]
    clean: bool,
    /// Enables verbose diagnostics.
    #[arg(short, long)]
    verbose: bool,
    /// Turns off all output.
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,
}

pub fn execute(cmd: Args) -> Result<(), Error> {
    let output = CommandOutput::from_quiet_and_verbose(cmd.quiet, cmd.verbose);
    let venv = ensure_self_venv(output)?;
    let project = PyProject::load_or_discover(cmd.pyproject.as_deref())?;

    let out = match cmd.out {
        Some(path) => path,
        None => project.workspace_path().join("dist"),
    };

    if cmd.clean {
        for entry in fs::read_dir(&out)? {
            let path = entry?.path();
            if path.is_file() {
                fs::remove_file(path)?;
            }
        }
    }

    let mut projects = Vec::new();

    if cmd.all {
        match project.workspace() {
            Some(workspace) => {
                for project in workspace.iter_projects() {
                    projects.push(project?);
                }
            }
            None => {
                projects.push(project);
            }
        }
    } else if cmd.package.is_empty() {
        projects.push(project);
    } else {
        for package_name in cmd.package {
            match project.workspace() {
                Some(workspace) => {
                    if let Some(project) = workspace.get_project(&package_name)? {
                        projects.push(project);
                    } else {
                        bail!("unknown project '{}'", package_name);
                    }
                }
                None => {
                    if project.normalized_name()? != normalize_package_name(&package_name) {
                        bail!("unknown project '{}'", package_name);
                    }
                }
            }
        }
    }

    for project in projects {
        if project.project_only() {
            continue;
        }
        if output != CommandOutput::Quiet {
            eprintln!("building {}", style(project.normalized_name()?).cyan());
        }

        let mut build_cmd = Command::new(get_venv_python_bin(&venv));
        build_cmd
            .arg("-mbuild")
            .env("NO_COLOR", "1")
            .arg("--outdir")
            .arg(&out)
            .arg(&*project.root_path());

        if cmd.wheel {
            build_cmd.arg("--wheel");
        }
        if cmd.sdist {
            build_cmd.arg("--sdist");
        }

        if output == CommandOutput::Quiet {
            build_cmd.stdout(Stdio::null());
            build_cmd.stderr(Stdio::null());
        }

        let status = build_cmd.status()?;
        if !status.success() {
            bail!("failed to build dist");
        }
    }

    Ok(())
}
