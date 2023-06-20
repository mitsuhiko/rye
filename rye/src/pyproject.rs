use clap::ValueEnum;
use core::fmt;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::env;
use std::env::consts::{ARCH, OS};
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Error};
use globset::GlobBuilder;
use once_cell::sync::Lazy;
use pep440_rs::{Operator, Version, VersionSpecifiers};
use pep508_rs::Requirement;
use regex::Regex;
use serde::Serialize;
use toml_edit::{Array, Document, Formatted, Item, Table, Value};
use url::Url;

use crate::config::Config;
use crate::consts::VENV_BIN;
use crate::platform::{get_python_version_request_from_pyenv_pin, list_known_toolchains};
use crate::sources::{get_download_url, matches_version, PythonVersion, PythonVersionRequest};
use crate::sync::VenvMarker;
use crate::utils::{
    expand_env_vars, format_requirement, get_short_executable_name, is_executable,
    reformat_toml_array_multiline,
};

static NORMALIZATION_SPLIT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[-_.]+").unwrap());

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DependencyKind<'a> {
    Normal,
    Dev,
    Excluded,
    Optional(Cow<'a, str>),
}

impl<'a> fmt::Display for DependencyKind<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DependencyKind::Normal => f.write_str("regular"),
            DependencyKind::Dev => f.write_str("dev"),
            DependencyKind::Excluded => f.write_str("excluded"),
            DependencyKind::Optional(ref sect) => write!(f, "optional ({})", sect),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DependencyRef {
    raw: String,
}

impl fmt::Display for DependencyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl DependencyRef {
    /// Creates a new dependency ref from the given string.
    pub fn new(s: &str) -> DependencyRef {
        DependencyRef { raw: s.to_string() }
    }

    /// Expands and parses the dependency ref into a requirement.
    ///
    /// The function is invoked for every referenced variable.
    pub fn expand<F>(&self, f: F) -> Result<Requirement, Error>
    where
        F: for<'a> FnMut(&'a str) -> Option<String>,
    {
        Ok(expand_env_vars(&self.raw, f).parse()?)
    }
}

/// Defines the type of the source reference.
#[derive(Copy, Clone, Debug)]
pub enum SourceRefType {
    Index,
    FindLinks,
}

impl FromStr for SourceRefType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "index" => Ok(SourceRefType::Index),
            "find-links" => Ok(SourceRefType::FindLinks),
            _ => Err(anyhow!("unknown source reference '{}'", s)),
        }
    }
}

/// Represents a source.
pub struct SourceRef {
    pub name: String,
    pub url: String,
    pub verify_ssl: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    pub ty: SourceRefType,
}

impl SourceRef {
    pub fn from_url(name: String, url: String, ty: SourceRefType) -> SourceRef {
        SourceRef {
            name,
            url,
            verify_ssl: true,
            username: None,
            password: None,
            ty,
        }
    }

    pub fn from_toml_table(source: &Table) -> Result<SourceRef, Error> {
        let name = source
            .get("name")
            .and_then(|x| x.as_str())
            .map(|x| x.to_string())
            .ok_or_else(|| anyhow!("expected name"))?;
        let url = source
            .get("url")
            .and_then(|x| x.as_str())
            .map(|x| x.to_string())
            .ok_or_else(|| anyhow!("expected url"))?;
        let verify_ssl = source
            .get("verify_ssl")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        let username = source
            .get("username")
            .and_then(|x| x.as_str())
            .map(|x| x.to_string());
        let password = source
            .get("password")
            .and_then(|x| x.as_str())
            .map(|x| x.to_string());
        let ty = source
            .get("type")
            .and_then(|x| x.as_str())
            .map_or(Ok(SourceRefType::Index), |x| x.parse::<SourceRefType>())
            .context("invalid value for type")?;
        Ok(SourceRef {
            name,
            url,
            verify_ssl,
            username,
            password,
            ty,
        })
    }

    /// Returns the URL with authentication expanded.
    ///
    /// This also fills in environment variables if there are any.
    pub fn expand_url(&self) -> Result<Url, Error> {
        let mut url =
            Url::parse(&expand_env_vars(&self.url, |name: &str| std::env::var(name).ok()) as &str)
                .context("invalid source url")?;
        if let Some(ref username) = self.username {
            url.set_username(username).ok();
        }
        if let Some(ref password) = self.password {
            url.set_password(Some(password)).ok();
        }
        Ok(url)
    }
}

type EnvVars = HashMap<String, String>;

/// A reference to a script
#[derive(Clone, Debug)]
pub enum Script {
    /// A command alias
    Cmd(Vec<String>, EnvVars),
    /// A multi-script execution
    Chain(Vec<Vec<String>>),
    /// External script reference
    External(PathBuf),
}

fn toml_array_as_string_array(arr: &Array) -> Vec<String> {
    arr.iter()
        .map(|x| {
            x.as_str()
                .map(|x| x.to_string())
                .unwrap_or_else(|| x.to_string())
        })
        .collect()
}

fn toml_value_as_command_args(value: &Value) -> Option<Vec<String>> {
    if let Some(cmd) = value.as_str() {
        shlex::split(cmd)
    } else {
        value.as_array().map(toml_array_as_string_array)
    }
}

impl Script {
    fn from_toml_item(item: &Item) -> Option<Script> {
        if let Some(detailed) = item.as_table_like() {
            if let Some(cmds) = detailed.get("chain").and_then(|x| x.as_array()) {
                Some(Script::Chain(
                    cmds.iter().flat_map(toml_value_as_command_args).collect(),
                ))
            } else if let Some(cmd) = detailed.get("cmd") {
                let cmd = toml_value_as_command_args(cmd.as_value()?)?;
                let env_vars = detailed
                    .get("env")
                    .and_then(|x| x.as_table_like())
                    .map(|x| {
                        x.iter()
                            .map(|x| {
                                (
                                    x.0.to_string(),
                                    x.1.as_str()
                                        .map(|x| x.to_string())
                                        .unwrap_or_else(|| x.1.to_string()),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(Script::Cmd(cmd, env_vars))
            } else {
                None
            }
        } else {
            toml_value_as_command_args(item.as_value()?)
                .map(|cmd| Script::Cmd(cmd, EnvVars::default()))
        }
    }
}

impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Script::Cmd(args, env) => {
                let mut need_space = false;
                for (key, value) in env.iter() {
                    if need_space {
                        write!(f, " ")?;
                    }
                    write!(f, "{}={}", shlex::quote(key), shlex::quote(value))?;
                    need_space = true;
                }
                for arg in args.iter() {
                    if need_space {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", shlex::quote(arg))?;
                    need_space = true;
                }
                Ok(())
            }
            Script::Chain(cmds) => {
                write!(f, "chain:")?;
                for (idx, cmd) in cmds.iter().enumerate() {
                    if idx > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, " [")?;
                    for (idx, arg) in cmd.iter().enumerate() {
                        if idx > 0 {
                            write!(f, " ")?;
                        }
                        write!(f, "{}", shlex::quote(arg))?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            Script::External(ref script) => write!(f, "external: {}", script.display()),
        }
    }
}

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    doc: Document,
    members: Option<Vec<String>>,
}

impl Workspace {
    /// Loads a workspace from a pyproject.toml and path
    fn try_load_from_toml(doc: &Document, path: &Path) -> Option<Workspace> {
        doc.get("tool")
            .and_then(|x| x.get("rye"))
            .and_then(|x| x.get("workspace"))
            .and_then(|x| x.as_table_like())
            .map(|workspace| Workspace {
                root: path.to_path_buf(),
                doc: doc.clone(),
                members: workspace
                    .get("members")
                    .and_then(|x| x.as_array())
                    .map(|x| {
                        x.iter()
                            .filter_map(|item| item.as_str().map(|x| x.to_string()))
                            .collect::<Vec<_>>()
                    }),
            })
    }

    /// Discovers a pyproject toml
    #[allow(unused)]
    pub fn discover_from_path(path: &Path) -> Option<Workspace> {
        let mut here = path;

        loop {
            let project_file = here.join("pyproject.toml");
            if project_file.is_file() {
                if let Ok(contents) = fs::read_to_string(&project_file) {
                    if let Ok(doc) = contents.parse::<Document>() {
                        if let Some(workspace) = Workspace::try_load_from_toml(&doc, here) {
                            return Some(workspace);
                        }
                    }
                }
            }

            here = match here.parent() {
                Some(parent) => parent,
                None => break,
            };
        }

        None
    }

    /// The path of the workspace
    pub fn path(&self) -> Cow<'_, Path> {
        Cow::Borrowed(&self.root)
    }

    /// Checks if a project is a member of the declared workspace.
    pub fn is_member(&self, path: &Path) -> bool {
        if let Ok(relative) = path.strip_prefix(&self.root) {
            if relative == Path::new("") {
                true
            } else {
                match &self.members {
                    None => true,
                    Some(members) => {
                        let path = relative.to_string_lossy();
                        for pattern in members {
                            let glob = GlobBuilder::new(pattern)
                                // backslash_escape=false for portability - same setting on all
                                // platforms
                                .literal_separator(true) // *,? do not match `/`
                                .backslash_escape(false) // backslash is never an escape character
                                .build();
                            match glob {
                                Ok(glob) => {
                                    if glob.compile_matcher().is_match(&*path) {
                                        return true;
                                    }
                                }
                                Err(err) => {
                                    eprintln!("warning: workspace.members: {}", err);
                                }
                            }
                        }
                        false
                    }
                }
            }
        } else {
            false
        }
    }

    /// Iterates through all projects in the workspace.
    pub fn iter_projects<'a>(
        self: &'a Arc<Self>,
    ) -> impl Iterator<Item = Result<PyProject, Error>> + 'a {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|entry| {
                !(entry.file_type().is_dir() && skip_recurse_into(entry.file_name()))
            })
            .filter_map(move |entry| match entry {
                Ok(entry) => {
                    if entry.file_type().is_file()
                        && entry.file_name() == OsStr::new("pyproject.toml")
                        && self.is_member(entry.path().parent().unwrap())
                    {
                        let project =
                            match PyProject::load_with_workspace(entry.path(), self.clone()) {
                                Ok(Some(project)) => project,
                                Ok(None) => return None,
                                Err(err) => return Some(Err(err)),
                            };
                        return Some(Ok(project));
                    }
                    None
                }
                Err(err) => Some(Err(err.into())),
            })
    }

    /// Looks up a single project.
    pub fn get_project<'a>(self: &'a Arc<Self>, p: &str) -> Result<Option<PyProject>, Error> {
        let normalized_name = normalize_package_name(p);
        for project in self.iter_projects() {
            let project = project?;
            if project.normalized_name()? == normalized_name {
                return Ok(Some(project));
            }
        }
        Ok(None)
    }

    /// Returns the virtualenv path of the workspace.
    pub fn venv_path(&self) -> Cow<'_, Path> {
        Cow::Owned(self.root.join(".venv"))
    }

    /// Returns the project's target python version.
    ///
    /// That is the Python version that appears as lower bound in the
    /// pyproject toml.
    pub fn target_python_version(&self) -> Option<PythonVersionRequest> {
        resolve_target_python_version(&self.doc, &self.root, &self.venv_path())
    }

    /// Returns the project's intended venv python version.
    ///
    /// This is the python version that should be used for virtualenvs.
    pub fn venv_python_version(&self) -> Result<PythonVersion, Error> {
        resolve_intended_venv_python_version(&self.doc, &self.root)
    }

    /// Returns a list of index URLs that should be considered.
    pub fn sources(&self) -> Result<Vec<SourceRef>, Error> {
        get_sources(&self.doc)
    }

    /// Is this workspace rye managed?
    pub fn rye_managed(&self) -> bool {
        is_rye_managed(&self.doc)
    }
}

/// Check if recurse should be skipped into directory with this name
fn skip_recurse_into(name: &OsStr) -> bool {
    // We want to ignore hidden directories: .venv, .git, and others.
    name.to_str().map(|s| s.starts_with('.')).unwrap_or(false)
}

/// Could not auto-discover any pyproject
#[derive(Debug, Clone)]
pub struct DiscoveryUnsuccessful;

impl std::error::Error for DiscoveryUnsuccessful {}

impl fmt::Display for DiscoveryUnsuccessful {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "did not find pyproject.toml")
    }
}

/// Helps working with pyproject.toml files
#[derive(Debug)]
pub struct PyProject {
    root: PathBuf,
    basename: OsString,
    workspace: Option<Arc<Workspace>>,
    doc: Document,
}

impl PyProject {
    /// Load a pyproject toml if explicitly given, else discover from current directory
    ///
    /// Used for command line arguments.
    pub fn load_or_discover(arg: Option<&Path>) -> Result<PyProject, Error> {
        match arg {
            // canonicalize because it comes from a command line argument
            Some(path) => Self::load(&path.canonicalize()?),
            None => Self::discover(),
        }
    }

    /// Discovers and loads a pyproject toml.
    pub fn discover() -> Result<PyProject, Error> {
        let pyproject_toml = match find_project_root() {
            Some(root) => root.join("pyproject.toml"),
            None => return Err(Error::from(DiscoveryUnsuccessful)),
        };
        Self::load(&pyproject_toml)
    }

    /// Loads a pyproject toml.
    pub fn load(filename: &Path) -> Result<PyProject, Error> {
        let root = filename.parent().unwrap_or(Path::new("."));
        let doc = fs::read_to_string(filename)
            .with_context(|| format!("failed to read pyproject.toml from {}", &filename.display()))?
            .parse::<Document>()
            .with_context(|| {
                format!(
                    "failed to parse pyproject.toml from {}",
                    &filename.display()
                )
            })?;
        let mut workspace = Workspace::try_load_from_toml(&doc, root).map(Arc::new);

        if workspace.is_none() {
            workspace = Workspace::discover_from_path(root).map(Arc::new);
        }

        if let Some(ref workspace) = workspace {
            if !workspace.is_member(root) {
                bail!(
                    "project {} is not part of pyproject workspace {}",
                    filename.display(),
                    workspace.path().display()
                );
            }
        }

        let basename = match filename.file_name() {
            Some(name) => name.to_os_string(),
            None => bail!("project {} has no file name", root.display()),
        };

        Ok(PyProject {
            root: root.to_owned(),
            basename,
            workspace,
            doc,
        })
    }

    /// Loads a pyproject toml with a given workspace.
    ///
    /// If the project is not a member of the workspace, `None` is returned.
    pub fn load_with_workspace(
        filename: &Path,
        workspace: Arc<Workspace>,
    ) -> Result<Option<PyProject>, Error> {
        let root = filename.parent().unwrap_or(Path::new("."));
        let doc = fs::read_to_string(filename)?
            .parse::<Document>()
            .with_context(|| {
                format!(
                    "failed to parse pyproject.toml from {} in context of workspace {}",
                    &filename.display(),
                    workspace.path().display(),
                )
            })?;

        if !workspace.is_member(root) {
            return Ok(None);
        }

        let basename = match filename.file_name() {
            Some(name) => name.to_os_string(),
            None => bail!("project {} has no file name", root.display()),
        };

        Ok(Some(PyProject {
            root: root.to_owned(),
            basename,
            workspace: Some(workspace),
            doc,
        }))
    }

    /// Returns a reference to the workspace.
    ///
    /// If something isn't a workspace, it's not returned.
    pub fn workspace(&self) -> Option<&Arc<Workspace>> {
        self.workspace.as_ref()
    }

    /// Is this the root project of the workspace?
    pub fn is_workspace_root(&self) -> bool {
        match self.workspace {
            Some(ref workspace) => workspace.path() == self.root_path(),
            None => true,
        }
    }

    /// Returns the project root path
    pub fn root_path(&self) -> Cow<'_, Path> {
        Cow::Borrowed(&self.root)
    }

    /// Returns the project workspace path.
    pub fn workspace_path(&self) -> Cow<'_, Path> {
        self.workspace
            .as_ref()
            .map(|x| x.path())
            .unwrap_or_else(|| self.root_path())
    }

    /// Returns the path to the toml file.
    pub fn toml_path(&self) -> Cow<'_, Path> {
        Cow::Owned(self.root.join(&self.basename))
    }

    /// Returns the location of the virtualenv.
    pub fn venv_path(&self) -> Cow<'_, Path> {
        match self.workspace() {
            Some(ws) => ws.venv_path(),
            None => self.root.join(".venv").into(),
        }
    }

    /// Returns the virtualenv bin path of the virtualenv.
    pub fn venv_bin_path(&self) -> Cow<'_, Path> {
        Cow::Owned(self.venv_path().join(VENV_BIN))
    }

    /// Returns the project's target python version
    pub fn target_python_version(&self) -> Option<PythonVersionRequest> {
        if let Some(workspace) = self.workspace() {
            workspace.target_python_version()
        } else {
            resolve_target_python_version(&self.doc, &self.root, &self.venv_path())
        }
    }

    /// Returns the project's intended venv python version.
    ///
    /// This is the python version that should be used for virtualenvs.
    pub fn venv_python_version(&self) -> Result<PythonVersion, Error> {
        if let Some(workspace) = self.workspace() {
            workspace.venv_python_version()
        } else {
            resolve_intended_venv_python_version(&self.doc, &self.root)
        }
    }

    /// Set the target Python version.
    pub fn set_target_python_version(&mut self, version: &PythonVersionRequest) {
        let mut marker = format!(">= {}", version.major);
        if let Some(minor) = version.minor {
            marker.push('.');
            marker.push_str(&minor.to_string());
        }

        let project = self
            .doc
            .entry("project")
            .or_insert(Item::Table(Table::new()));
        project["requires-python"] = Item::Value(Value::String(Formatted::new(marker)));
    }

    /// Set the project version.
    pub fn set_version(&mut self, version: &Version) {
        let project = self
            .doc
            .entry("project")
            .or_insert(Item::Table(Table::new()));

        project["version"] = Item::Value(Value::String(Formatted::new(version.to_string())));
    }

    /// Returns the project name.
    pub fn name(&self) -> Option<&str> {
        self.doc
            .get("project")
            .and_then(|x| x.get("name"))
            .and_then(|x| x.as_str())
    }

    /// Returns the normalized name.
    pub fn normalized_name(&self) -> Result<String, Error> {
        self.name()
            .map(normalize_package_name)
            .ok_or_else(|| anyhow!("project from '{}' has no name", self.root_path().display()))
    }

    /// Returns the version.
    pub fn version(&mut self) -> Result<Version, Error> {
        let version = self
            .doc
            .get("project")
            .and_then(|x| x.get("version"))
            .and_then(|x| x.as_str());

        match version {
            Some(version) => {
                Version::from_str(version).map_err(|msg| anyhow!("invalid version: {}", msg))
            }
            None => {
                let version = Version::from_str("0.1.0").unwrap();
                self.set_version(&version);
                self.save()?;

                Ok(version)
            }
        }
    }

    /// Returns the build backend.
    pub fn build_backend(&self) -> Option<BuildSystem> {
        let backend = self
            .doc
            .get("build-system")
            .and_then(|x| x.get("build-backend"))
            .and_then(|x| x.as_str());
        match backend {
            Some("hatchling.build") => Some(BuildSystem::Hatchling),
            Some("setuptools.build_meta") => Some(BuildSystem::Setuptools),
            Some("flit_core.buildapi") => Some(BuildSystem::Flit),
            Some("pdm.backend") => Some(BuildSystem::Pdm),
            _ => None,
        }
    }
    /// Looks up a script
    pub fn get_script_cmd(&self, key: &str) -> Option<Script> {
        let external = self.venv_bin_path().join(key);
        if is_executable(&external) && !is_unsafe_script(&external) {
            Some(Script::External(external))
        } else {
            Script::from_toml_item(
                self.doc
                    .get("tool")
                    .and_then(|x| x.get("rye"))
                    .and_then(|x| x.get("scripts"))
                    .and_then(|x| x.get(key))?,
            )
        }
    }

    /// Returns a list of known scripts.
    pub fn list_scripts(&self) -> HashSet<String> {
        let mut rv = match self
            .doc
            .get("tool")
            .and_then(|x| x.get("rye"))
            .and_then(|x| x.get("scripts"))
            .and_then(|x| x.as_table_like())
        {
            Some(tbl) => tbl.iter().map(|x| x.0.to_string()).collect(),
            None => HashSet::new(),
        };
        for entry in fs::read_dir(&self.venv_bin_path())
            .ok()
            .into_iter()
            .flatten()
            .flatten()
        {
            if is_executable(&entry.path()) && !is_unsafe_script(&entry.path()) {
                rv.insert(get_short_executable_name(&entry.path()));
            }
        }
        rv
    }

    /// Returns a set of all extras.
    pub fn extras(&self) -> HashSet<&str> {
        self.doc
            .get("project")
            .and_then(|x| x.get("optional-dependencies"))
            .and_then(|x| x.as_table_like())
            .map_or(None.into_iter(), |x| {
                Some(x.iter().map(|x| x.0)).into_iter()
            })
            .flatten()
            .collect()
    }

    /// Adds a dependency.
    pub fn add_dependency(
        &mut self,
        req: &Requirement,
        kind: &DependencyKind,
    ) -> Result<(), Error> {
        let dependencies = match kind {
            DependencyKind::Normal => &mut self.doc["project"]["dependencies"],
            DependencyKind::Dev => &mut self.doc["tool"]["rye"]["dev-dependencies"],
            DependencyKind::Excluded => &mut self.doc["tool"]["rye"]["excluded-dependencies"],
            DependencyKind::Optional(ref section) => {
                // add this as a proper non-inline table if it's missing
                let table = &mut self.doc["project"]["optional-dependencies"];
                if table.is_none() {
                    *table = Item::Table(Table::new());
                }
                &mut table[section as &str]
            }
        };
        if dependencies.is_none() {
            *dependencies = Item::Value(Value::Array(Array::new()));
        }
        set_dependency(
            dependencies
                .as_array_mut()
                .ok_or_else(|| anyhow!("dependencies in pyproject.toml are malformed"))?,
            req,
        );
        Ok(())
    }

    /// Removes a dependency
    pub fn remove_dependency(
        &mut self,
        req: &Requirement,
        kind: DependencyKind,
    ) -> Result<Option<Requirement>, Error> {
        let dependencies = match kind {
            DependencyKind::Normal => &mut self.doc["project"]["dependencies"],
            DependencyKind::Dev => &mut self.doc["tool"]["rye"]["dev-dependencies"],
            DependencyKind::Excluded => &mut self.doc["tool"]["rye"]["excluded-dependencies"],
            DependencyKind::Optional(ref section) => {
                &mut self.doc["project"]["optional-dependencies"][section as &str]
            }
        };
        if !dependencies.is_none() {
            Ok(remove_dependency(
                dependencies
                    .as_array_mut()
                    .ok_or_else(|| anyhow!("dependencies in pyproject.toml are malformed"))?,
                req,
            ))
        } else {
            Ok(None)
        }
    }

    /// Iterates over all dependencies.
    pub fn iter_dependencies(
        &self,
        kind: DependencyKind,
    ) -> impl Iterator<Item = DependencyRef> + '_ {
        let sec = match kind {
            DependencyKind::Normal => self.doc.get("project").and_then(|x| x.get("dependencies")),
            DependencyKind::Dev => self
                .doc
                .get("tool")
                .and_then(|x| x.get("rye"))
                .and_then(|x| x.get("dev-dependencies")),
            DependencyKind::Excluded => self
                .doc
                .get("tool")
                .and_then(|x| x.get("rye"))
                .and_then(|x| x.get("excluded-dependencies")),
            DependencyKind::Optional(ref section) => self
                .doc
                .get("project")
                .and_then(|x| x.get("optional-dependencies"))
                .and_then(|x| x.get(section as &str)),
        };
        sec.and_then(|x| x.as_array())
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str())
            .map(DependencyRef::new)
    }

    /// Returns a list of sources that should be considered.
    pub fn sources(&self) -> Result<Vec<SourceRef>, Error> {
        match self.workspace {
            Some(ref workspace) => workspace.sources(),
            None => get_sources(&self.doc),
        }
    }

    /// Is this project rye managed?
    pub fn rye_managed(&self) -> bool {
        match self.workspace {
            Some(ref workspace) => workspace.rye_managed(),
            None => is_rye_managed(&self.doc),
        }
    }

    /// Save back changes
    pub fn save(&self) -> Result<(), Error> {
        fs::write(self.toml_path(), self.doc.to_string()).with_context(|| {
            format!("unable to write changes to {}", self.toml_path().display())
        })?;
        Ok(())
    }

    /// Is this only a workspace, not a package?
    pub fn project_only(&self) -> bool {
        is_project_only(&self.doc)
    }
}

pub fn normalize_package_name(x: &str) -> String {
    NORMALIZATION_SPLIT_RE
        .split(x)
        .fold(String::new(), |mut acc, item| {
            if !acc.is_empty() {
                acc.push('-');
            }
            acc.push_str(&item.to_ascii_lowercase());
            acc
        })
}

fn set_dependency(deps: &mut Array, req: &Requirement) {
    let mut to_replace = None;
    for (idx, dep) in deps.iter().enumerate() {
        if let Some(dep) = dep.as_str() {
            if let Ok(dep_req) = Requirement::from_str(dep) {
                if dep_req.name.eq_ignore_ascii_case(&req.name) {
                    to_replace = Some(idx);
                    break;
                }
            }
        }
    }

    let formatted = format_requirement(req).to_string();
    if let Some(idx) = to_replace {
        deps.replace(idx, formatted);
    } else {
        deps.push(formatted);
    }
    reformat_toml_array_multiline(deps);
}

fn remove_dependency(deps: &mut Array, req: &Requirement) -> Option<Requirement> {
    let mut to_remove = None;
    for (idx, dep) in deps.iter().enumerate() {
        if let Some(dep) = dep.as_str() {
            if let Ok(dep_req) = Requirement::from_str(dep) {
                if dep_req.name.eq_ignore_ascii_case(&req.name) {
                    to_remove = Some(idx);
                    break;
                }
            }
        }
    }

    if let Some(idx) = to_remove {
        let rv = deps
            .remove(idx)
            .as_str()
            .and_then(|x| Requirement::from_str(x).ok());
        reformat_toml_array_multiline(deps);
        rv
    } else {
        None
    }
}

pub fn get_current_venv_python_version(venv_path: &Path) -> Option<PythonVersion> {
    let marker_file = venv_path.join("rye-venv.json");
    let contents = fs::read(marker_file).ok()?;
    let marker: VenvMarker = serde_json::from_slice(&contents).ok()?;
    Some(marker.python)
}

/// Give a given python version request, returns the latest available version.
///
/// This can return a version that requires downloading.
pub fn latest_available_python_version(
    requested_version: &PythonVersionRequest,
) -> Option<PythonVersion> {
    let mut all = if let Ok(available) = list_known_toolchains() {
        available
            .into_iter()
            .filter_map(|(ver, _)| {
                if matches_version(requested_version, &ver) {
                    Some(ver)
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    if let Some((latest, _, _)) = get_download_url(requested_version, OS, ARCH) {
        all.push(latest);
    };

    all.sort();
    all.into_iter().rev().next()
}

fn resolve_target_python_version(
    doc: &Document,
    root: &Path,
    venv_path: &Path,
) -> Option<PythonVersionRequest> {
    resolve_lower_bound_python_version(doc)
        .or_else(|| get_current_venv_python_version(venv_path).map(Into::into))
        .or_else(|| get_python_version_request_from_pyenv_pin(root).map(Into::into))
        .or_else(|| Config::current().default_toolchain().ok())
}

fn resolve_intended_venv_python_version(
    doc: &Document,
    root: &Path,
) -> Result<PythonVersion, Error> {
    let requested_version = get_python_version_request_from_pyenv_pin(root)
        .or_else(|| resolve_lower_bound_python_version(doc))
        .or_else(|| Config::current().default_toolchain().ok())
        .ok_or_else(|| {
            anyhow!(
                "could not determine a target python version.  Define requires-python in \
                 pyproject.toml or use a .python-version file"
            )
        })?;

    if let Ok(ver) = PythonVersion::try_from(requested_version.clone()) {
        return Ok(ver);
    }

    if let Some(latest) = latest_available_python_version(&requested_version) {
        Ok(latest)
    } else {
        Err(anyhow!(
            "Unable to determine target virtualenv python version"
        ))
    }
}

fn resolve_lower_bound_python_version(doc: &Document) -> Option<PythonVersionRequest> {
    doc.get("project")
        .and_then(|x| x.get("requires-python"))
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<VersionSpecifiers>().ok())
        .and_then(|versions| {
            versions
                .iter()
                .filter(|x| {
                    matches!(
                        x.operator(),
                        Operator::Equal
                            | Operator::EqualStar
                            | Operator::GreaterThanEqual
                            | Operator::GreaterThan
                    )
                })
                .map(|x| {
                    let mut rv = PythonVersionRequest::from(x.version().clone());
                    // this is pretty shitty, but probably good enough
                    if matches!(x.operator(), Operator::GreaterThan) {
                        if let Some(ref mut patch) = rv.patch {
                            *patch += 1;
                        } else if let Some(ref mut minor) = rv.minor {
                            *minor += 1;
                        }
                    }
                    rv
                })
                .min()
        })
}

pub fn find_project_root() -> Option<PathBuf> {
    let mut here = env::current_dir().ok()?;

    loop {
        let project_file = here.join("pyproject.toml");
        if project_file.is_file() {
            return Some(here.to_path_buf());
        }

        if !here.pop() {
            break;
        }
    }

    None
}

fn is_unsafe_script(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let stem = path.file_stem();
        stem == Some(OsStr::new("activate")) || stem == Some(OsStr::new("deactivate"))
    }
    #[cfg(unix)]
    {
        // this is done because on unix pypy throws a bunch of dylibs into the bin folder
        path.extension() == Some(OsStr::new("dylib"))
    }
}

fn get_sources(doc: &Document) -> Result<Vec<SourceRef>, Error> {
    let cfg = Config::current();
    let mut rv = Vec::new();

    if let Some(sources) = doc
        .get("tool")
        .and_then(|x| x.get("rye"))
        .and_then(|x| x.get("sources"))
        .and_then(|x| x.as_array_of_tables())
    {
        for source in sources {
            let source_ref = SourceRef::from_toml_table(source)?;
            rv.push(source_ref);
        }
    }

    let mut seen = HashSet::<String>::from_iter(rv.iter().map(|x| x.name.clone()));

    for source in cfg.sources()? {
        if !seen.contains(&source.name) {
            seen.insert(source.name.clone());
            rv.push(source);
        }
    }

    Ok(rv)
}

fn is_rye_managed(doc: &Document) -> bool {
    if Config::current().force_rye_managed() {
        return true;
    }
    doc.get("tool")
        .and_then(|x| x.get("rye"))
        .and_then(|x| x.get("managed"))
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

fn is_project_only(doc: &Document) -> bool {
    doc.get("tool")
        .and_then(|x| x.get("rye"))
        .and_then(|x| x.get("project-only"))
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

/// Represents expanded sources.
#[derive(Debug, Clone, Serialize)]
pub struct ExpandedSources {
    pub index_urls: Vec<Url>,
    pub find_links: Vec<Url>,
    pub trusted_hosts: HashSet<String>,
}

impl ExpandedSources {
    /// Takes some sources and expands them.
    pub fn from_sources(sources: &[SourceRef]) -> Result<ExpandedSources, Error> {
        let mut index_urls = Vec::new();
        let mut find_links = Vec::new();
        let mut trusted_hosts = HashSet::new();

        for source in sources {
            let url = source.expand_url()?;
            if !source.verify_ssl {
                if let Some(host) = url.host_str() {
                    trusted_hosts.insert(host.to_string());
                }
            }
            match source.ty {
                SourceRefType::Index => index_urls.push(url),
                SourceRefType::FindLinks => find_links.push(url),
            }
        }

        Ok(ExpandedSources {
            index_urls,
            find_links,
            trusted_hosts,
        })
    }

    /// Attach common pip args to a command.
    pub fn add_as_pip_args(&self, cmd: &mut Command) {
        for (idx, url) in self.index_urls.iter().enumerate() {
            if idx == 0 {
                cmd.arg("--index-url");
            } else {
                cmd.arg("--extra-index-url");
            }
            cmd.arg(&url.to_string());
        }
        for link in &self.find_links {
            cmd.arg("--find-links");
            cmd.arg(&link.to_string());
        }
        for host in &self.trusted_hosts {
            cmd.arg("--trusted-host");
            cmd.arg(host);
        }
    }
}

#[derive(ValueEnum, Copy, Clone, Serialize, Debug, PartialEq)]
#[value(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BuildSystem {
    Hatchling,
    Setuptools,
    Flit,
    Pdm,
}

impl FromStr for BuildSystem {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "hatchling" => Ok(BuildSystem::Hatchling),
            "setuptools" => Ok(BuildSystem::Setuptools),
            "flit" => Ok(BuildSystem::Flit),
            "pdm" => Ok(BuildSystem::Pdm),
            _ => Err(anyhow!("unknown build system")),
        }
    }
}
