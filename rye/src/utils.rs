use std::borrow::Cow;
use std::io::Cursor;
use std::path::Path;
use std::{fmt, fs};

use anyhow::Error;
use once_cell::sync::Lazy;
use pep508_rs::{Requirement, VersionOrUrl};
use regex::{Captures, Regex};

static ENV_VAR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\$\{([A-Z0-9_]+)\}").unwrap());

#[derive(Debug)]
pub struct QuietExit(pub i32);

impl std::error::Error for QuietExit {}

impl fmt::Display for QuietExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exit with {}", self.0)
    }
}

/// Controls the fetch output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum CommandOutput {
    /// Regular output
    #[default]
    Normal,
    /// Extra verbose output
    Verbose,
    /// No output
    Quiet,
}

impl CommandOutput {
    /// Returns the preferred command output for those flags.
    pub fn from_quiet_and_verbose(quiet: bool, verbose: bool) -> CommandOutput {
        if quiet {
            CommandOutput::Quiet
        } else if verbose {
            CommandOutput::Verbose
        } else {
            CommandOutput::Normal
        }
    }
}

/// Formats a Python requirement.
pub fn format_requirement(req: &Requirement) -> impl fmt::Display + '_ {
    struct Helper<'x>(&'x Requirement);

    impl<'x> fmt::Display for Helper<'x> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0.name)?;
            if let Some(extras) = &self.0.extras {
                write!(f, "[{}]", extras.join(","))?;
            }
            if let Some(version_or_url) = &self.0.version_or_url {
                match version_or_url {
                    VersionOrUrl::VersionSpecifier(version_specifier) => {
                        let version_specifier: Vec<String> =
                            version_specifier.iter().map(ToString::to_string).collect();
                        write!(f, "{}", version_specifier.join(", "))?;
                    }
                    VersionOrUrl::Url(url) => {
                        // retain `{` and `}` for interpolation in URLs
                        write!(
                            f,
                            " @ {}",
                            url.to_string().replace("%7B", "{").replace("%7D", "}")
                        )?;
                    }
                }
            }
            if let Some(marker) = &self.0.marker {
                write!(f, " ; {}", marker)?;
            }
            Ok(())
        }
    }

    Helper(req)
}

/// Helper to expand envvars
pub fn expand_env_vars<F>(string: &str, mut f: F) -> Cow<'_, str>
where
    F: for<'a> FnMut(&'a str) -> Option<String>,
{
    ENV_VAR_RE.replace_all(string, |m: &Captures| f(&m[1]).unwrap_or_default())
}

/// Unpacks a tarball.
///
/// Today this assumes that the tarball is zstd compressed which happens
/// to be what the indygreg python builds use.
pub fn unpack_tarball(contents: &[u8], dst: &Path, strip_components: usize) -> Result<(), Error> {
    let reader = Cursor::new(contents);
    let decoder = zstd::stream::read::Decoder::with_buffer(reader)?;
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?;
        let mut components = name.components();
        for _ in 0..strip_components {
            components.next();
        }
        let path = dst.join(components.as_path());

        // only unpack if it's save to do so
        if path != Path::new("") && path.strip_prefix(dst).is_ok() {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).ok();
            }
            entry.unpack(&path)?;
        }
    }
    Ok(())
}

// TODO(cnpryer)
pub mod auth {
    use std::ops::Deref;

    use anyhow::Error;
    use ring::aead::*;

    /// A `Secret` struct modeled after Cargo's `Secret` wrapper, including `ring`-backed
    /// encryption and decryption methods.
    ///
    /// TODO(cnpryer): Could split into Secret and RegistryConfig or something.
    pub struct Secret<T> {
        inner: T,
    }

    impl<T> Secret<T> {
        pub fn encrypt_with_key(&self, key: &[u8], nonce: [u8; 12]) -> Result<Vec<u8>, Error>
        where
            T: ToString,
        {
            let mut data = self.inner.to_string().as_bytes().to_vec();
            let key = UnboundKey::new(&CHACHA20_POLY1305, key).unwrap();
            let owned_nonce = Nonce::assume_unique_for_key(nonce);
            let key = LessSafeKey::new(key);
            key.seal_in_place_append_tag(owned_nonce, Aad::empty(), &mut data)
                .unwrap();

            Ok(data.to_vec())
        }

        pub fn decrypt_with_key(&self, key: &[u8], nonce: [u8; 12]) -> Option<Vec<u8>>
        where
            T: ToString,
        {
            let mut data = self.inner.to_string().as_bytes().to_vec();
            let key = UnboundKey::new(&CHACHA20_POLY1305, key).unwrap();
            let owned_nonce = Nonce::assume_unique_for_key(nonce);
            let key = LessSafeKey::new(key);
            let data = key
                .open_in_place(owned_nonce, Aad::empty(), &mut data)
                .unwrap();

            Some(data.to_vec())
        }
    }

    impl<T> From<T> for Secret<T> {
        fn from(inner: T) -> Self {
            Self { inner }
        }
    }

    impl<T> Deref for Secret<T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
}

#[test]
fn test_quiet_exit_display() {
    let quiet_exit = QuietExit(0);
    assert_eq!("exit with 0", format!("{}", quiet_exit));
}

#[cfg(test)]
mod test_format_requirement {
    use super::{format_requirement, Requirement};

    #[test]
    fn test_format_requirement_simple() {
        let req: Requirement = "foo>=1.0.0".parse().unwrap();
        assert_eq!("foo>=1.0.0", format_requirement(&req).to_string());
    }

    #[test]
    fn test_format_requirement_complex() {
        let req: Requirement = "foo[extra1,extra2]>=1.0.0,<2.0.0; python_version<'3.8'"
            .parse()
            .unwrap();
        assert_eq!(
            "foo[extra1,extra2]>=1.0.0, <2.0.0 ; python_version < '3.8'",
            format_requirement(&req).to_string()
        );
    }
    #[test]
    fn test_format_requirement_file_path() {
        // this support is just for generating dependencies.  Parsing such requirements
        // is only partially supported as expansion has to happen before parsing.
        let req: Requirement = "foo @ file:///${PROJECT_ROOT}/foo".parse().unwrap();
        assert_eq!(
            format_requirement(&req).to_string(),
            "foo @ file:///${PROJECT_ROOT}/foo"
        );
    }
}

#[cfg(test)]
mod test_command_output {
    use super::CommandOutput;

    #[test]
    fn test_command_output_defaults() {
        assert_eq!(CommandOutput::Normal, CommandOutput::default());
    }

    #[test]
    fn test_command_output_from_quiet_and_verbose() {
        let quiet = true;
        let verbose = true;

        assert_eq!(
            CommandOutput::Quiet,
            CommandOutput::from_quiet_and_verbose(quiet, false)
        );
        assert_eq!(
            CommandOutput::Verbose,
            CommandOutput::from_quiet_and_verbose(false, verbose)
        );
        assert_eq!(
            CommandOutput::Normal,
            CommandOutput::from_quiet_and_verbose(false, false)
        );
        assert_eq!(
            CommandOutput::Quiet,
            CommandOutput::from_quiet_and_verbose(quiet, verbose)
        ); // Quiet takes precedence over verbose
    }
}

#[cfg(test)]
mod test_expand_env_vars {
    use super::expand_env_vars;

    #[test]
    fn test_expand_env_vars_no_expansion() {
        let input = "This string has no env vars";
        let output = expand_env_vars(input, |_| None);
        assert_eq!(input, output);
    }

    #[test]
    fn test_expand_env_vars_with_expansion() {
        let input = "This string has an env var: ${EXAMPLE_VAR}";
        let output = expand_env_vars(input, |var| {
            if var == "EXAMPLE_VAR" {
                Some("Example value".to_string())
            } else {
                None
            }
        });
        assert_eq!("This string has an env var: Example value", output);
    }
}
