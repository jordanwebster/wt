use wt_core::env::ACTIVATION_KEY;
use wt_core::report::{BinReport, EnvData, PortReport};
use wt_core::CoreError;

use crate::cli::Env;

use super::{door, Context, Output};

pub(crate) fn run(context: &mut Context, args: Env) -> Result<Output, CoreError> {
    if args.deactivate {
        let deactivated = wt_core::deactivate(&context.parent_env)?;
        let text = if args.sh {
            let mut lines = vec![format!("unset {ACTIVATION_KEY}")];
            if let Some(activation) = deactivated.prior {
                for key in deactivated.report.restored {
                    match activation.prior.get(&key).and_then(|value| value.as_ref()) {
                        Some(value) => lines.push(format!("export {key}={}", quote(value))),
                        None => lines.push(format!("unset {key}")),
                    }
                }
            }
            lines.join("\n")
        } else {
            deactivated
                .clean
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        return Output::text(serde_json::json!({"deactivated": true}), text);
    }
    let prior_activation = wt_core::deactivate(&context.parent_env)?.prior;
    let door = door::enter(context, args.target.as_deref(), "env")?;
    let bins = door
        .config
        .root
        .bin
        .iter()
        .map(|relative| {
            let path = std::path::Path::new(door.tree.path.as_str()).join(relative.as_str());
            let exists = matches!(
                wt_sys::fsx::path_kind(&path),
                Ok(wt_sys::fsx::PathKind::Directory)
            );
            let executables = if exists {
                wt_sys::fsx::read_dir_paths(&path)?
                    .into_iter()
                    .filter(|path| wt_sys::fsx::is_executable_file(path).unwrap_or(false))
                    .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
                    .collect()
            } else {
                Vec::new()
            };
            Ok(BinReport {
                dir: path.to_string_lossy().into_owned(),
                exists,
                executables,
            })
        })
        .collect::<Result<Vec<_>, CoreError>>()?;
    let data = EnvData {
        target: door.target.to_string(),
        set: door.env.report.set.clone(),
        overrode: door.env.report.overrode.clone(),
        restored: door.env.report.restored.clone(),
        missing_bins: door.env.report.missing_bins.clone(),
        rendered: door
            .env
            .render
            .iter()
            .map(|render| render.path.clone())
            .collect(),
        bins,
        ports: {
            let mut ports = door
                .identity
                .ports
                .iter()
                .map(|(name, index)| PortReport {
                    name: name.as_str().to_owned(),
                    port: door.identity.geometry.port_base + u16::from(*index),
                    bound: None,
                })
                .collect::<Vec<_>>();
            ports.sort_by_key(|port| {
                door.identity.ports[&wt_core::model::PortName::new(&port.name).unwrap()]
            });
            ports
        },
        env: door.env.env.clone(),
        activation: door.env.activation.clone(),
    };
    let text = if args.sh {
        let mut lines = door
            .env
            .env
            .iter()
            .map(|(key, value)| format!("export {key}={}", quote(value)))
            .collect::<Vec<_>>();
        if let Some(prior) = prior_activation {
            for key in prior.applied.keys() {
                if prior.prior[key].is_none() && !door.env.activation.applied.contains_key(key) {
                    lines.push(format!("unset {key}"));
                }
            }
        }
        lines.join("\n")
    } else if args.dotenv {
        door.env
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        let mut lines = door
            .env
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>();
        lines.push(String::new());
        lines.push("bin inventory:".to_owned());
        lines.extend(data.bins.iter().map(|bin| {
            format!(
                "{}\t{}\t{}",
                bin.dir,
                if bin.exists { "present" } else { "missing" },
                bin.executables.join(",")
            )
        }));
        lines.push(String::new());
        lines.push("ports:".to_owned());
        lines.extend(
            data.ports
                .iter()
                .map(|port| format!("{}\t{}", port.name, port.port)),
        );
        if !data.overrode.is_empty() {
            lines.push(String::new());
            lines.push("overrides:".to_owned());
            lines.extend(data.overrode.iter().map(|key| {
                let applied = &data.activation.applied[key];
                let prior = data.activation.prior[key].as_deref().unwrap_or("<unset>");
                format!("{key}\tapplied={applied}\tprior={prior}")
            }));
        }
        lines.join("\n")
    };
    Ok(Output::text(data, text)?.with_notices(door.notices))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
