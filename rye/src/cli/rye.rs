use anyhow::{bail, Context, Error, Ok};
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::process::Command;

use crate::platform::get_app_dir;

/// Rye self management
#[derive(Parser, Debug)]
pub struct Args {
    #[command(subcommand)]
    command: SubCommand,
}

/// Generates a completion script for a shell.
#[derive(Parser, Debug)]
pub struct CompletionCommand {
    /// The shell to generate a completion script for (defaults to 'bash').
    #[arg(short, long)]
    shell: Option<Shell>,
    /// Install completion script to shell.
    #[arg(long)]
    install: bool,
}

/// Performs an update of rye.
///
/// This currently just is an alias to running cargo install again with the
/// right arguments.
#[derive(Parser, Debug)]
pub struct UpdateCommand {
    /// Update to a specific tag.
    #[arg(long)]
    tag: Option<String>,
    /// Update to a specific git rev.
    #[arg(long, conflicts_with = "tag")]
    rev: Option<String>,
    /// Force reinstallation
    #[arg(long)]
    force: bool,
}

#[derive(Parser, Debug)]
enum SubCommand {
    Completion(CompletionCommand),
    Update(UpdateCommand),
}

pub fn execute(cmd: Args) -> Result<(), Error> {
    match cmd.command {
        SubCommand::Completion(args) => completion(args),
        SubCommand::Update(args) => update(args),
    }
}

fn completion(args: CompletionCommand) -> Result<(), Error> {
    let shell = args.shell.unwrap_or(Shell::Bash);
    if !args.install {
        clap_complete::generate(
            shell,
            &mut super::Args::command(),
            "rye",
            &mut std::io::stdout(),
        );
        return Ok(());
    }
    install_completion(shell)?;
    Ok(())
}

fn update(args: UpdateCommand) -> Result<(), Error> {
    let mut helper = rename_helper::RenameHelper::new()?;
    let mut cmd = Command::new("cargo");
    cmd.arg("install")
        .arg("--git")
        .arg("https://github.com/mitsuhiko/rye");
    if let Some(ref rev) = args.rev {
        cmd.arg("--rev");
        cmd.arg(rev);
    } else if let Some(ref tag) = args.tag {
        cmd.arg("--tag");
        cmd.arg(tag);
    }
    if args.force {
        cmd.arg("--force");
    }
    cmd.arg("rye");
    let status = cmd.status().context("unable to update via cargo-install")?;
    if !status.success() {
        bail!("failed to self-update via cargo-install");
    }
    helper.disarm();

    Ok(())
}

fn install_completion(shell: Shell) -> Result<(), Error> {
    let completion_dir = get_app_dir().join("completion");
    fs::create_dir_all(&completion_dir)?;

    let shell_map = HashMap::from([
        (Shell::Bash, ("rye-completion.bash", ".bash_profile")),
        (Shell::Zsh, ("_rye", ".zshrc")),
        (Shell::Fish, ("rye.fish", ".config/fish/completions")),
    ]);

    if !shell_map.contains_key(&shell) {
        bail!("Unsupported install completion for this shell");
    }
    let shell_completion_file = completion_dir.join(shell_map[&shell].0);
    // generate completion script
    clap_complete::generate(
        shell,
        &mut super::Args::command(),
        "rye",
        &mut fs::File::create(&shell_completion_file)?,
    );

    let shell_config_file = simple_home_dir::home_dir()
        .unwrap()
        .join(shell_map[&shell].1);

    // fish shell completion is a bit special
    if shell == Shell::Fish {
        fs::create_dir_all(shell_config_file.clone())?;
        fs::copy(
            shell_completion_file.clone(),
            shell_config_file.join("rye.fish"),
        )?;
        eprintln!(
            "enabled completion to {}, if not work, run this: \n cp {} {}",
            shell,
            shell_completion_file.display(),
            shell_config_file.join("rye.fish").display()
        );
        return Ok(());
    }

    let enable_cmd = match shell {
        Shell::Bash => format!("\nsource {}\n", shell_completion_file.display()),
        Shell::Zsh => format!(
            "\nfpath=($fpath {}) && compinit\n",
            completion_dir.display()
        ),
        _ => String::new(),
    };
    // write completion script to shell config file
    let mut shell_config_fs = fs::OpenOptions::new()
        .append(true)
        .read(true)
        .create(true)
        .open(shell_config_file)?;

    let mut shell_config = String::new();
    shell_config_fs.read_to_string(&mut shell_config)?;
    if !shell_config.contains(&enable_cmd) {
        shell_config_fs
            .write(enable_cmd.as_bytes())
            .context("failed to install shell completion")?;
    }

    eprintln!(
        "enabled completion to {}, if not work, add this to your shell config:\n {}",
        shell, enable_cmd
    );

    Ok(())
}

#[cfg(windows)]
mod rename_helper {
    use super::*;
    use std::{env, fs, path::PathBuf};

    pub struct RenameHelper {
        original_path: PathBuf,
        path: PathBuf,
        disarmed: bool,
    }

    impl RenameHelper {
        pub fn new() -> Result<RenameHelper, Error> {
            let original_path = env::current_exe()?;
            let path = original_path.with_extension("tmp");
            fs::rename(&original_path, &path)?;
            Ok(RenameHelper {
                original_path,
                path,
                disarmed: false,
            })
        }

        pub fn disarm(&mut self) {
            self.disarmed = true;
        }
    }

    impl Drop for RenameHelper {
        fn drop(&mut self) {
            if !self.disarmed {
                fs::rename(&self.path, &self.original_path).ok();
            }
        }
    }
}

#[cfg(unix)]
mod rename_helper {
    use super::*;
    pub struct RenameHelper;

    impl RenameHelper {
        pub fn new() -> Result<RenameHelper, Error> {
            Ok(RenameHelper)
        }

        pub fn disarm(&mut self) {}
    }
}
