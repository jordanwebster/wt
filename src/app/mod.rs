mod adopt;
mod clone_repo;
mod close;
mod completions;
mod config;
mod context;
mod destroy;
mod doctor;
mod door;
mod env;
mod exec;
mod executor;
mod human;
mod list;
mod locks;
mod new;
mod open;
mod path;
mod prune;
mod refresh;
mod register;
mod remove;
mod run;
mod shell;
mod shell_init;
mod status;
mod sync;
mod tasks;
mod unregister;
mod which;

use std::io::Write;

use serde::Serialize;
use serde_json::Value;
use wt_core::report::{canonical_json, Envelope, Notice};
use wt_core::CoreError;

use crate::cli::{Cli, Command};

pub(crate) use context::Context;

pub(crate) struct Output {
    pub data: Value,
    pub text: Option<String>,
    pub notices: Vec<Notice>,
    after_render: Option<AfterRender>,
    failure: Option<CoreError>,
}

pub(crate) enum AfterRender {
    Attach { session: String },
    NewSession { target: String },
}

impl Output {
    pub fn data(data: impl Serialize) -> Result<Self, CoreError> {
        let data = serde_json::to_value(data).map_err(internal_serialize)?;
        Ok(Self {
            data,
            text: None,
            notices: Vec::new(),
            after_render: None,
            failure: None,
        })
    }

    pub fn text(data: impl Serialize, text: impl Into<String>) -> Result<Self, CoreError> {
        let mut output = Self::data(data)?;
        output.text = Some(text.into());
        Ok(output)
    }

    pub fn with_notices(mut self, notices: impl IntoIterator<Item = Notice>) -> Self {
        self.notices.extend(notices);
        self.notices.sort_by(|left, right| {
            (&left.level, &left.code, &left.subject, &left.message).cmp(&(
                &right.level,
                &right.code,
                &right.subject,
                &right.message,
            ))
        });
        self.notices
            .dedup_by(|left, right| left.code == right.code && left.subject == right.subject);
        self
    }

    pub fn with_failure(mut self, error: CoreError) -> Self {
        self.failure = Some(error);
        self
    }

    pub fn after_render(mut self, action: AfterRender) -> Self {
        self.after_render = Some(action);
        self
    }
}

pub fn main(cli: Cli) -> i32 {
    let command = cli.command.name().to_owned();
    let human_kind = human::HumanKind::from(&cli.command);
    let json = cli.json;
    let verbose = cli.verbose;
    let quiet = cli.quiet;
    let stderr_tty = wt_sys::snapshot::tty().stderr;
    let color = matches!(cli.color, crate::cli::Color::Always)
        || (matches!(cli.color, crate::cli::Color::Auto) && stderr_tty);
    let standalone = match &cli.command {
        Command::ShellInit(args) => Some(shell_init::generate(args.shell)),
        Command::Completions(args) => Some(completions::generate(args.shell)),
        _ => None,
    };
    let mut opened_context = None;
    let (result, pending_notices) = if let Some(result) = standalone {
        (result, Vec::new())
    } else {
        match Context::open(&cli) {
            Ok(mut context) => {
                let result = dispatch(&mut context, cli);
                let notices = std::mem::take(&mut context.pending_notices);
                opened_context = Some(context);
                (result, notices)
            }
            Err(error) => (Err(error), Vec::new()),
        }
    };
    match result {
        Ok(mut output) => {
            let output_exit = output
                .failure
                .as_ref()
                .map_or(0, |error| i32::from(error.exit()));
            if json {
                let mut envelope = if let Some(error) = output.failure.clone() {
                    Envelope::partial_failure(
                        command.clone(),
                        env!("CARGO_PKG_VERSION"),
                        output.data,
                        error,
                    )
                } else {
                    Envelope::success(command.clone(), env!("CARGO_PKG_VERSION"), output.data)
                };
                envelope.notices.append(&mut output.notices);
                write_stdout(canonical_json(&envelope).unwrap_or_else(|_| "{}".to_owned()));
            } else {
                for notice in &output.notices {
                    if !matches!(
                        notice.code.as_str(),
                        "BIN_DIR_MISSING" | "SESSION_BACKEND_SELECTED"
                    ) && !quiet
                        && (stderr_tty || verbose)
                    {
                        let code = if color {
                            format!("\u{1b}[33m{}\u{1b}[0m", notice.code)
                        } else {
                            notice.code.clone()
                        };
                        let _ = writeln!(std::io::stderr(), "wt: {} — {}", code, notice.message);
                    }
                }
                if matches!(human_kind, human::HumanKind::Run) {
                    let guidance = human::with_expected_next(String::new(), &output.notices);
                    if !guidance.is_empty() {
                        let _ = writeln!(std::io::stderr(), "{guidance}");
                    }
                }
                let text = output
                    .text
                    .map(|text| {
                        if matches!(human_kind, human::HumanKind::Run) {
                            text
                        } else {
                            human::with_expected_next(text, &output.notices)
                        }
                    })
                    .unwrap_or_else(|| human_kind.render(&output.data, &output.notices));
                write_stdout(text);
            }
            let _ = std::io::stdout().flush();
            if let Some(action) = output.after_render {
                let context = opened_context
                    .as_mut()
                    .expect("deferred actions require an open context");
                match action {
                    AfterRender::NewSession { target } => {
                        if let Err(error) = open::open_new_after_summary(context, &target) {
                            emit_session_warning(&target, &error);
                        }
                    }
                    AfterRender::Attach { session } => {
                        if let Err(error) = open::attach(context, &session) {
                            return render_error(&command, json, color, error, Vec::new());
                        }
                    }
                }
            }
            output_exit
        }
        Err(error) => render_error_with_visibility(
            &command,
            json,
            color,
            error,
            pending_notices,
            quiet,
            stderr_tty,
            verbose,
        ),
    }
}

fn emit_session_warning(target: &str, error: &CoreError) {
    let _ = writeln!(
        std::io::stderr(),
        "wt: SESSION_CREATE_FAILED — session for {target} was not created: {}; run `wt open {target}` to retry",
        error.message
    );
}

fn render_error(
    command: &str,
    json: bool,
    color: bool,
    error: CoreError,
    notices: Vec<Notice>,
) -> i32 {
    render_error_with_visibility(command, json, color, error, notices, false, true, false)
}

#[allow(clippy::too_many_arguments)]
fn render_error_with_visibility(
    command: &str,
    json: bool,
    color: bool,
    error: CoreError,
    pending_notices: Vec<Notice>,
    quiet: bool,
    stderr_tty: bool,
    verbose: bool,
) -> i32 {
    let exit = if !json
        && matches!(command, "run" | "test" | "lint" | "fmt" | "build")
        && error.code.0 == "TASK_FAILED"
    {
        error.details["child"]["code"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok())
            .or_else(|| {
                error.details["child"]["signal"]
                    .as_i64()
                    .and_then(|signal| i32::try_from(signal).ok())
                    .map(|signal| 128 + signal)
            })
            .unwrap_or_else(|| i32::from(error.exit()))
    } else {
        i32::from(error.exit())
    };
    if json {
        let mut envelope =
            Envelope::<Value>::failure(command.to_owned(), env!("CARGO_PKG_VERSION"), error);
        envelope.notices = pending_notices;
        write_stdout(canonical_json(&envelope).unwrap_or_else(|_| "{}".to_owned()));
    } else {
        for notice in pending_notices {
            if !matches!(
                notice.code.as_str(),
                "BIN_DIR_MISSING" | "SESSION_BACKEND_SELECTED"
            ) && !quiet
                && (stderr_tty || verbose)
            {
                let _ = writeln!(
                    std::io::stderr(),
                    "wt: {} — {}",
                    notice.code,
                    notice.message
                );
            }
        }
        let code = if color {
            format!("\u{1b}[31m{}\u{1b}[0m", error.code.0)
        } else {
            error.code.0.clone()
        };
        let _ = writeln!(std::io::stderr(), "wt: {} — {}", code, error.message);
        let _ = writeln!(std::io::stderr(), "remedy: {}", error.remedy);
    }
    exit
}

fn dispatch(context: &mut Context, cli: Cli) -> Result<Output, CoreError> {
    match cli.command {
        Command::Register(args) => register::run(context, args),
        Command::Unregister(args) => unregister::run(context, args),
        Command::Clone(args) => clone_repo::run(context, args),
        Command::New(args) => new::run(context, args),
        Command::Adopt(args) => adopt::run(context, args),
        Command::List(args) => list::run(context, args),
        Command::Status(args) => status::run(context, args),
        Command::Path(args) => path::run(context, args),
        Command::Run(args) => run::run(context, args),
        Command::Sync(args) => sync::run(context, args),
        Command::Test(args) => run::run(context, args.into_run("test")),
        Command::Lint(args) => run::run(context, args.into_run("lint")),
        Command::Fmt(args) => run::run(context, args.into_run("fmt")),
        Command::Build(args) => run::run(context, args.into_run("build")),
        Command::Exec(args) => exec::run(context, args),
        Command::Shell(args) => shell::run(context, args),
        Command::Env(args) => env::run(context, args),
        Command::Open(args) => open::run(context, args),
        Command::Close(args) => close::run(context, args),
        Command::Remove(args) => remove::run(context, args),
        Command::Prune(args) => prune::run(context, args),
        Command::Destroy(args) => destroy::run(context, args),
        Command::Refresh(args) => refresh::run(context, args),
        Command::Doctor(args) => doctor::run(context, args),
        Command::Tasks(args) => tasks::run(context, args),
        Command::Config(args) => config::run(context, args),
        Command::Which(args) => which::run(context, args),
        Command::Locks(args) => locks::run(context, args),
        Command::ShellInit(args) => shell_init::run(context, args),
        Command::Completions(args) => completions::run(context, args),
    }
}

fn internal_serialize(error: serde_json::Error) -> CoreError {
    CoreError::new(
        wt_core::ExitClass::Internal,
        "SERIALIZE_FAILED",
        error.to_string(),
        "report this wt bug",
    )
}

fn write_stdout(mut text: String) {
    if text.is_empty() {
        return;
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let _ = std::io::stdout().write_all(text.as_bytes());
}
