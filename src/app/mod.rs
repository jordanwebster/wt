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
}

impl Output {
    pub fn data(data: impl Serialize) -> Result<Self, CoreError> {
        let data = serde_json::to_value(data).map_err(internal_serialize)?;
        Ok(Self {
            data,
            text: None,
            notices: Vec::new(),
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
    let (result, pending_notices) = if let Some(result) = standalone {
        (result, Vec::new())
    } else {
        match Context::open(&cli) {
            Ok(mut context) => {
                let result = dispatch(&mut context, cli);
                (result, context.pending_notices)
            }
            Err(error) => (Err(error), Vec::new()),
        }
    };
    match result {
        Ok(mut output) => {
            if json {
                let mut envelope =
                    Envelope::success(command, env!("CARGO_PKG_VERSION"), output.data);
                envelope.notices.append(&mut output.notices);
                write_stdout(canonical_json(&envelope).unwrap_or_else(|_| "{}".to_owned()));
            } else {
                for notice in &output.notices {
                    if notice.code != "BIN_DIR_MISSING" && !quiet && (stderr_tty || verbose) {
                        let code = if color {
                            format!("\u{1b}[33m{}\u{1b}[0m", notice.code)
                        } else {
                            notice.code.clone()
                        };
                        let _ = writeln!(std::io::stderr(), "wt: {} — {}", code, notice.message);
                    }
                }
                let text = output
                    .text
                    .map(|text| human::with_expected_next(text, &output.notices))
                    .unwrap_or_else(|| human_kind.render(&output.data, &output.notices));
                write_stdout(text);
            }
            0
        }
        Err(error) => {
            let exit = if !json
                && matches!(command.as_str(), "run" | "test" | "lint" | "fmt" | "build")
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
                    Envelope::<Value>::failure(command, env!("CARGO_PKG_VERSION"), error);
                envelope.notices = pending_notices;
                write_stdout(canonical_json(&envelope).unwrap_or_else(|_| "{}".to_owned()));
            } else {
                for notice in pending_notices {
                    if notice.code != "BIN_DIR_MISSING" && !quiet && (stderr_tty || verbose) {
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
    }
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
