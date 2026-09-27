use super::{DesktopApplication, executable_available};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

/// A local path or already encoded URI selected for launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchTarget {
    Local(PathBuf),
    Uri(Box<str>),
}

impl LaunchTarget {
    #[must_use]
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local(path.into())
    }

    pub fn uri(uri: impl Into<Box<str>>) -> Result<Self, LaunchError> {
        let uri = uri.into();
        if valid_uri(&uri) {
            Ok(Self::Uri(uri))
        } else {
            Err(LaunchError::InvalidUri)
        }
    }
}

/// Configured terminal prefix used for entries that declare `Terminal=true`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalCommand {
    program: OsString,
    arguments: Vec<OsString>,
}

impl TerminalCommand {
    pub fn new(
        program: impl Into<OsString>,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Result<Self, LaunchError> {
        let program = program.into();
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        if !valid_os_argument(&program)
            || arguments
                .iter()
                .any(|argument| !valid_os_argument(argument))
        {
            return Err(LaunchError::InvalidTerminalCommand);
        }
        Ok(Self { program, arguments })
    }
}

/// Exact program invocation. It is never rendered as a shell command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedLaunch {
    program: OsString,
    arguments: Vec<OsString>,
    working_directory: Option<PathBuf>,
    /// For a checked program, the file that was opened and checked; the
    /// program runs from it, not from `program`, its path (SYS-035).
    checked: Option<CheckedExecutable>,
}

/// The open file a checked program runs from. Two launches are equal only
/// when they hold the same open file.
#[derive(Clone, Debug)]
struct CheckedExecutable(Arc<OwnedFd>);

impl PartialEq for CheckedExecutable {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CheckedExecutable {}

impl PreparedLaunch {
    /// Execute one absolute local file directly, with no shell or implicit arguments.
    pub fn for_executable_file(path: &Path) -> Result<Self, LaunchError> {
        if !path.is_absolute() || !valid_os_argument(path.as_os_str()) {
            return Err(LaunchError::InvalidExecutable(path.to_path_buf()));
        }
        let working_directory = path
            .parent()
            .ok_or_else(|| LaunchError::InvalidExecutable(path.to_path_buf()))?;
        validate_working_directory(Some(working_directory))?;
        Ok(Self {
            program: path.as_os_str().to_os_string(),
            arguments: Vec::new(),
            working_directory: Some(working_directory.to_path_buf()),
            checked: None,
        })
    }

    /// Run the compiled program open as `file`, whose path is `path`, from
    /// that open file, with no shell or implicit arguments and its folder
    /// as the working directory (SYS-035). The program's name is `path`.
    pub(crate) fn for_checked_program(path: &Path, file: OwnedFd) -> Result<Self, LaunchError> {
        Ok(Self {
            checked: Some(CheckedExecutable(Arc::new(file))),
            ..Self::for_executable_file(path)?
        })
    }

    /// Run `program` with `arguments` in `working_directory`, as given.
    pub(crate) fn command(
        program: &Path,
        arguments: Vec<OsString>,
        working_directory: &Path,
    ) -> Result<Self, LaunchError> {
        if !program.is_absolute()
            || !valid_os_argument(program.as_os_str())
            || arguments
                .iter()
                .any(|argument| !valid_os_argument(argument))
        {
            return Err(LaunchError::InvalidExecutable(program.to_path_buf()));
        }
        validate_working_directory(Some(working_directory))?;
        Ok(Self {
            program: program.as_os_str().to_os_string(),
            arguments,
            working_directory: Some(working_directory.to_path_buf()),
            checked: None,
        })
    }

    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    #[must_use]
    pub fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedLaunches {
    launches: Vec<PreparedLaunch>,
}

impl PreparedLaunches {
    #[must_use]
    pub fn launches(&self) -> &[PreparedLaunch] {
        &self.launches
    }
}

/// Injectable process boundary for deterministic launch tests.
pub trait ProcessRunner: Send + Sync {
    fn spawn(&self, launch: &PreparedLaunch) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn spawn(&self, launch: &PreparedLaunch) -> io::Result<()> {
        // A checked program runs from its open file, through a duplicate the
        // child inherits (a duplicate has no close-on-exec flag), so a file
        // put at its path after the check cannot run. A child another thread
        // starts at the same moment may inherit the duplicate too; it only
        // holds the program's file open for reading.
        let inherited = launch
            .checked
            .as_ref()
            .map(|checked| rustix::io::dup(&*checked.0).map_err(io::Error::from))
            .transpose()?;
        let mut command = match &inherited {
            Some(file) => {
                let mut command = Command::new(format!("/proc/self/fd/{}", file.as_raw_fd()));
                command.arg0(&launch.program);
                command
            }
            None => Command::new(&launch.program),
        };
        command
            .args(&launch.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        if let Some(directory) = &launch.working_directory {
            command.current_dir(directory);
        }
        command.spawn().map(|_| ())
    }
}

#[derive(Clone, Debug)]
pub struct DesktopEntryLauncher {
    executable_directories: Vec<PathBuf>,
}

impl DesktopEntryLauncher {
    #[must_use]
    pub fn new(executable_directories: Vec<PathBuf>) -> Self {
        Self {
            executable_directories,
        }
    }

    pub fn prepare(
        &self,
        application: &DesktopApplication,
        targets: &[LaunchTarget],
        terminal: Option<&TerminalCommand>,
    ) -> Result<PreparedLaunches, LaunchError> {
        if let Some(try_exec) = application.try_exec()
            && !executable_available(try_exec, &self.executable_directories)
        {
            return Err(LaunchError::TryExecUnavailable(try_exec.to_os_string()));
        }
        validate_working_directory(application.working_directory())?;
        let tokens = parse_exec(application.exec())?;
        let input_code = validate_field_codes(&tokens)?;
        let converted = convert_targets(input_code, targets)?;
        let target_sets: Vec<&[OsString]> = match input_code {
            Some('f' | 'u') if converted.is_empty() => vec![&[]],
            Some('f' | 'u') => converted.iter().map(std::slice::from_ref).collect(),
            _ => vec![converted.as_slice()],
        };
        let launches = target_sets
            .into_iter()
            .map(|targets| build_launch(application, &tokens, input_code, targets, terminal))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PreparedLaunches { launches })
    }

    /// Prepare a desktop entry at a caller-selected local working directory.
    /// The directory remains process metadata and is never interpolated into
    /// the entry's `Exec` arguments.
    pub fn prepare_in_working_directory(
        &self,
        application: &DesktopApplication,
        targets: &[LaunchTarget],
        terminal: Option<&TerminalCommand>,
        working_directory: &Path,
    ) -> Result<PreparedLaunches, LaunchError> {
        let mut prepared = self.prepare(application, targets, terminal)?;
        for launch in &mut prepared.launches {
            launch.working_directory = Some(working_directory.to_path_buf());
        }
        Ok(prepared)
    }

    pub fn launch(
        &self,
        prepared: &PreparedLaunches,
        runner: &(impl ProcessRunner + ?Sized),
    ) -> Result<(), LaunchError> {
        for launch in prepared.launches() {
            runner.spawn(launch).map_err(LaunchError::Spawn)?;
        }
        Ok(())
    }
}

fn build_launch(
    application: &DesktopApplication,
    tokens: &[ExecToken],
    input_code: Option<char>,
    targets: &[OsString],
    terminal: Option<&TerminalCommand>,
) -> Result<PreparedLaunch, LaunchError> {
    let scalar_target = matches!(input_code, Some('f' | 'u'))
        .then(|| targets.first())
        .flatten();
    let mut argv = Vec::new();
    for token in tokens {
        if matches!(token.value.as_str(), "%F" | "%U") {
            argv.extend(targets.iter().cloned());
            continue;
        }
        if token.value == "%i" {
            if let Some(icon) = application.icon() {
                argv.push(OsString::from("--icon"));
                argv.push(icon.as_os_str().to_os_string());
            }
            continue;
        }
        if let Some(argument) = expand_token(token, application, scalar_target)? {
            argv.push(argument);
        }
    }
    if argv.is_empty() || !valid_os_argument(&argv[0]) {
        return Err(LaunchError::InvalidExec);
    }
    let program = argv.remove(0);
    let mut launch = PreparedLaunch {
        program,
        arguments: argv,
        working_directory: application.working_directory().map(Path::to_path_buf),
        checked: None,
    };
    if application.terminal() {
        let terminal = terminal.ok_or(LaunchError::TerminalUnavailable)?;
        let mut arguments = terminal.arguments.clone();
        arguments.push(launch.program);
        arguments.extend(launch.arguments);
        launch = PreparedLaunch {
            program: terminal.program.clone(),
            arguments,
            working_directory: launch.working_directory,
            checked: None,
        };
    }
    Ok(launch)
}

fn expand_token(
    token: &ExecToken,
    application: &DesktopApplication,
    target: Option<&OsString>,
) -> Result<Option<OsString>, LaunchError> {
    let mut result = OsString::new();
    let mut literal = String::new();
    let mut chars = token.value.chars();
    let mut expanded = false;
    while let Some(character) = chars.next() {
        if character != '%' {
            literal.push(character);
            continue;
        }
        let Some(code) = chars.next() else {
            return Err(LaunchError::InvalidFieldCode);
        };
        result.push(&literal);
        literal.clear();
        match code {
            '%' => result.push("%"),
            'f' | 'u' => {
                if let Some(target) = target {
                    result.push(target);
                    expanded = true;
                }
            }
            'c' => {
                result.push(application.name());
                expanded = true;
            }
            'k' => {
                result.push(application.desktop_file());
                expanded = true;
            }
            'd' | 'D' | 'n' | 'N' | 'v' | 'm' => {}
            'F' | 'U' | 'i' => return Err(LaunchError::InvalidFieldCode),
            _ => return Err(LaunchError::InvalidFieldCode),
        }
    }
    result.push(literal);
    if result.is_empty() && !token.quoted && !expanded {
        Ok(None)
    } else {
        Ok(Some(result))
    }
}

#[derive(Debug)]
struct ExecToken {
    value: String,
    quoted: bool,
}

fn parse_exec(exec: &str) -> Result<Vec<ExecToken>, LaunchError> {
    if exec.is_empty() || !exec.is_ascii() || exec.chars().any(|character| character == '\0') {
        return Err(LaunchError::InvalidExec);
    }
    let mut tokens = Vec::new();
    let mut value = String::new();
    let mut chars = exec.chars().peekable();
    let mut quoted = false;
    let mut token_quoted = false;
    let mut closed_quote = false;
    while let Some(character) = chars.next() {
        if quoted {
            match character {
                '"' => {
                    quoted = false;
                    closed_quote = true;
                }
                '\\' => {
                    let escaped = chars.next().ok_or(LaunchError::InvalidExec)?;
                    if !matches!(escaped, '"' | '`' | '$' | '\\') {
                        return Err(LaunchError::InvalidExec);
                    }
                    value.push(escaped);
                }
                '\n' | '\r' => return Err(LaunchError::InvalidExec),
                _ => value.push(character),
            }
            continue;
        }
        if closed_quote && !character.is_ascii_whitespace() {
            return Err(LaunchError::InvalidExec);
        }
        if character.is_ascii_whitespace() {
            if !value.is_empty() || token_quoted {
                tokens.push(ExecToken {
                    value: std::mem::take(&mut value),
                    quoted: token_quoted,
                });
                token_quoted = false;
                closed_quote = false;
            }
            continue;
        }
        if character == '"' {
            if !value.is_empty() || token_quoted {
                return Err(LaunchError::InvalidExec);
            }
            quoted = true;
            token_quoted = true;
            continue;
        }
        if matches!(
            character,
            '\'' | '\\'
                | '>'
                | '<'
                | '~'
                | '|'
                | '&'
                | ';'
                | '$'
                | '*'
                | '?'
                | '#'
                | '('
                | ')'
                | '`'
        ) {
            return Err(LaunchError::InvalidExec);
        }
        value.push(character);
    }
    if quoted {
        return Err(LaunchError::InvalidExec);
    }
    if !value.is_empty() || token_quoted {
        tokens.push(ExecToken {
            value,
            quoted: token_quoted,
        });
    }
    if tokens.is_empty() || tokens[0].value.contains('=') {
        return Err(LaunchError::InvalidExec);
    }
    Ok(tokens)
}

fn validate_field_codes(tokens: &[ExecToken]) -> Result<Option<char>, LaunchError> {
    let mut input_code = None;
    for (token_index, token) in tokens.iter().enumerate() {
        let mut chars = token.value.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '%' {
                continue;
            }
            let code = chars.next().ok_or(LaunchError::InvalidFieldCode)?;
            if token.quoted {
                return Err(LaunchError::InvalidFieldCode);
            }
            match code {
                '%' | 'c' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm' => {}
                'i' if token.value == "%i" && token_index > 0 => {}
                'F' | 'U' if token.value == format!("%{code}") && token_index > 0 => {
                    if input_code.replace(code).is_some() {
                        return Err(LaunchError::InvalidFieldCode);
                    }
                }
                'f' | 'u' if token_index > 0 => {
                    if input_code.replace(code).is_some() {
                        return Err(LaunchError::InvalidFieldCode);
                    }
                }
                _ => return Err(LaunchError::InvalidFieldCode),
            }
        }
    }
    Ok(input_code)
}

fn convert_targets(
    input_code: Option<char>,
    targets: &[LaunchTarget],
) -> Result<Vec<OsString>, LaunchError> {
    let Some(input_code) = input_code else {
        return Ok(Vec::new());
    };
    let mut converted = Vec::with_capacity(targets.len());
    let mut refusals = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let conversion = match input_code {
            'f' | 'F' => local_argument(target),
            'u' | 'U' => uri_argument(target),
            _ => unreachable!("validated input code"),
        };
        match conversion {
            Some(argument) => converted.push(argument),
            None => refusals.push(LaunchRefusal {
                index,
                required: if matches!(input_code, 'f' | 'F') {
                    LaunchBoundary::LocalPath
                } else {
                    LaunchBoundary::Uri
                },
            }),
        }
    }
    if refusals.is_empty() {
        Ok(converted)
    } else {
        Err(LaunchError::UnrepresentableTargets(refusals))
    }
}

fn local_argument(target: &LaunchTarget) -> Option<OsString> {
    let LaunchTarget::Local(path) = target else {
        return None;
    };
    (path.is_absolute() && valid_os_argument(path.as_os_str())).then(|| path.as_os_str().into())
}

fn uri_argument(target: &LaunchTarget) -> Option<OsString> {
    match target {
        LaunchTarget::Uri(uri) => Some(uri.as_ref().into()),
        LaunchTarget::Local(path) => {
            let path = path.to_str()?;
            if !path.starts_with('/') {
                return None;
            }
            let mut uri = String::from("file://");
            for byte in path.bytes() {
                if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                    uri.push(char::from(byte));
                } else {
                    use std::fmt::Write as _;
                    write!(uri, "%{byte:02X}").expect("writing to a string cannot fail");
                }
            }
            Some(uri.into())
        }
    }
}

fn valid_uri(uri: &str) -> bool {
    let Some((scheme, remainder)) = uri.split_once(':') else {
        return false;
    };
    !remainder.is_empty()
        && scheme
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
        && !uri
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn validate_working_directory(directory: Option<&Path>) -> Result<(), LaunchError> {
    if let Some(directory) = directory
        && (!directory.is_absolute()
            || !valid_os_argument(directory.as_os_str())
            || !directory.is_dir())
    {
        return Err(LaunchError::InvalidWorkingDirectory(
            directory.to_path_buf(),
        ));
    }
    Ok(())
}

fn valid_os_argument(argument: &OsStr) -> bool {
    !argument.is_empty() && !argument.as_bytes().contains(&0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchBoundary {
    LocalPath,
    Uri,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchRefusal {
    index: usize,
    required: LaunchBoundary,
}

impl LaunchRefusal {
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    #[must_use]
    pub const fn required(&self) -> LaunchBoundary {
        self.required
    }
}

#[derive(Debug)]
pub enum LaunchError {
    InvalidUri,
    InvalidExec,
    InvalidExecutable(PathBuf),
    InvalidFieldCode,
    InvalidTerminalCommand,
    InvalidWorkingDirectory(PathBuf),
    TryExecUnavailable(OsString),
    TerminalUnavailable,
    UnrepresentableTargets(Vec<LaunchRefusal>),
    Spawn(io::Error),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUri => formatter.write_str("invalid launch URI"),
            Self::InvalidExec => formatter.write_str("invalid desktop entry Exec value"),
            Self::InvalidExecutable(path) => {
                write!(formatter, "invalid executable file: {}", path.display())
            }
            Self::InvalidFieldCode => formatter.write_str("invalid desktop entry field code"),
            Self::InvalidTerminalCommand => formatter.write_str("invalid terminal command"),
            Self::InvalidWorkingDirectory(path) => {
                write!(
                    formatter,
                    "invalid launch working directory: {}",
                    path.display()
                )
            }
            Self::TryExecUnavailable(executable) => write!(
                formatter,
                "desktop entry TryExec is unavailable: {}",
                Path::new(executable).display()
            ),
            Self::TerminalUnavailable => {
                formatter.write_str("desktop entry requires a configured terminal")
            }
            Self::UnrepresentableTargets(refusals) => write!(
                formatter,
                "{} launch target(s) cannot cross the required path/URI boundary",
                refusals.len()
            ),
            Self::Spawn(source) => write!(formatter, "application launch failed: {source}"),
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(source) => Some(source),
            _ => None,
        }
    }
}
