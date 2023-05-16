# Changelog

There is currently no actual release of Rye.  The latest main branch revision
is the one you can install as mainline should always be stable.  Here are the
most recent changes however.

## May
- Added `--install` for `rye self completion` to install the completion script
  directly into the shell's configuration.

- Rye now includes a `publish` command for publishing Python packages to a
  package repository.  #86

- Script declarations in `pyproject.toml` now permit chaining and custom
  environment variables.  #153

- Added `tools install` and `tools uninstall` as aliases for `install` and
  `uninstall` and added `tools list` to show all installed tools.

- Rye is now capable of downloading a selected set of PyPy releases.  To do
  so use `rye pin pypy@3.9.16` or any other supported PyPy release.

- Custom cpython toolchains are now registered just as `cpython` rather
  than `custom-cpython`.

- Rye now supports Python down to 3.7.

- Rye's `self` command now includes a `completion` subcommand to generate
  a completion script for your shell.

- The downloaded Python distributions are now validated against the
  SHA-256 hashes.

- Rye now builds on windows.  This is even more experimental though
  than support for Linux and macOS.

- Added `--features` and `--all-features` for `lock` and `sync`.

- Rye will now look at the `RYE_HOME` to determine the location of the
  `.rye` folder.  If it's not set, `$HOME/.rye` is used as before.

- Rye now has a most consistent handling for virtualenv versions.  If
  `.python-version` is provided, that version is used.  Otherwise if
  `requires-python` is set in the `pyproject.toml`, that version is used
  instead.  When a new project is created the `.python-version` file is
  written and the current latest cpython version is picked.

- It's now possible to explicitly set the `name` of the project when
  initializing a new one.

- Rye's `init` command now attempts to initialize projects with `git` and
  will automatically create a `src/project_name/__init__.py` file.

- Rye can now also generate a license text when initializing projects.

## April

- Rye now supports negative (exclusion) dependencies.  These can be used to
  prevent a dependency from installing, even if something else in the graph
  depends on it.  Use `rye add --exclude package-name` to add such a dependency.

- `sync` now accepts `--no-lock` to prevent updating the lock file.

- Rye's `add` command now accepts a `--pre` parameter to include pre-release.

- Rye's `pin` command now updates the pyproject.toml requires-python.

- Rye's `install` command now accepts a `--include-dep` parameter to include
  scripts from one or more given dependencies.

- Rye now honors `requires-python` in the `add` command.  This means the the
  initial resolution will not pick a version higher than what's supported by
  the lower boundary.

- When installing packages as global tools, a warning is now emitted if there
  were no scripts in the package.  Additionally installing packages from local
  paths and zip files is now supported.

- A `rye self update` command was added to compile and install the latest
  version via cargo.

- Added more convenient ways to install from git/urls by supplying a `--git`
  or `--url` parameter.  This will behind the scenes format a PEP 508 requirement
  string.

- Added a `shell` command which will spawn a shell with the virtualenv activated.

- Added a `make-req` command to conveniently format out PEP 508 requirement
  strings from parts.

- The internal virtualenv used to manage `pip-tools` and other libraries now
  automatically updates when necessary.

- `rye toolchain register` can now be used to register a local python installation
  as toolchain with rye.

- `rye build` was added to allow building `sdist` and `bdist_wheel` distributions.

- Rye now correctly handles whitespace in folder names.
