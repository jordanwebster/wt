use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use wt_core::{CoreError, ExitClass};
use wt_sys::proc::{CommandRequest, Tee};

use crate::cli::With;

use super::{door, executor, Context, Output};

type InstanceLease = (wt_core::resource::ResourceKey, std::path::PathBuf, u64);

pub(crate) fn run(context: &mut Context, args: With) -> Result<Output, CoreError> {
    let door = door::enter(context, None, "with")?;
    let config = wt_core::config::effective_scope(&door.config, ".")?;
    let mut seen = BTreeSet::new();
    let mut durable = Vec::new();
    let mut pools = Vec::new();
    for name in &args.resources {
        if !seen.insert(name.clone()) {
            return Err(CoreError::new(
                ExitClass::Usage,
                "RESOURCE_DUPLICATE",
                format!("resource `{name}` was named more than once"),
                "name each resource once",
            ));
        }
        let resource = config.resources.get(name).ok_or_else(|| {
            CoreError::new(
                ExitClass::NotFound,
                "RESOURCE_NOT_FOUND",
                format!("resource `{name}` is not declared"),
                "declare it in `[resource]` or correct the name",
            )
        })?;
        if resource.token_pool() || resource.count.is_some() {
            pools.push((name.clone(), resource.clone()));
            continue;
        }
        let plan = executor::plan(context, &door, name)?;
        let execution = executor::execute_plan(
            context,
            &door,
            &plan,
            executor::ExecuteOptions {
                take: args.take,
                ..executor::ExecuteOptions::DEFAULT
            },
        )?;
        durable.push(execution.data);
    }
    if args.cmd.is_empty() && pools.iter().any(|(_, pool)| pool.token_pool()) {
        return Err(CoreError::new(
            ExitClass::Usage,
            "TOKEN_COMMAND_REQUIRED",
            format!("token pool `{}` requires a command", pools[0].0),
            format!("run `wt with {}` -- <command>", pools[0].0),
        ));
    }
    if args.cmd.is_empty() {
        return Output::data(serde_json::json!({
            "target": door.target.to_string(),
            "resources": args.resources,
            "ensured": durable,
            "leases": [],
            "child": null,
            "log": null,
        }));
    }

    pools.sort_by(|left, right| left.0.cmp(&right.0));
    let mut lease_env = BTreeMap::new();
    let mut guards = Vec::new();
    let mut instance_leases: Vec<InstanceLease> = Vec::new();
    let holder = context.holder(door.target.to_string(), "with")?;
    macro_rules! leased_try {
        ($result:expr) => {
            match $result {
                Ok(value) => value,
                Err(error) => {
                    finish_instance_leases(context, &door, &instance_leases, false)?;
                    drop(guards);
                    return Err(error);
                }
            }
        };
    }
    for (name, pool) in &pools {
        let env_key = lease_key(name);
        if lease_env.contains_key(&env_key) {
            finish_instance_leases(context, &door, &instance_leases, false)?;
            drop(guards);
            return Err(CoreError::new(
                ExitClass::State,
                "CONFIG_INVALID",
                format!("pool names in this lease collide as `{env_key}`"),
                "rename one pool so its lease environment key is distinct",
            ));
        }
        let wait = pool.wait.as_deref().unwrap_or("30m");
        let wait = (wait != "forever").then(|| {
            Duration::from_millis(
                wt_core::model::duration_millis(wait).expect("validated pool wait"),
            )
        });
        let dir = lease_dir(context, &door, name, pool.tied_to);
        let guard = leased_try!(acquire_slot(
            context, &door, name, pool, &dir, &holder, wait
        ));
        let slot = guard.slot() + 1;
        let lease_name = if pool.token_pool() {
            format!("{name}-{slot}")
        } else {
            let id = format!("{name}#{slot}");
            let plan = leased_try!(executor::plan(context, &door, &id));
            let execution = leased_try!(executor::execute_plan(
                context,
                &door,
                &plan,
                executor::ExecuteOptions {
                    take: args.take,
                    ..executor::ExecuteOptions::DEFAULT
                },
            ));
            durable.push(execution.data);
            let node = plan
                .nodes
                .iter()
                .find(|node| node.id == id)
                .expect("pool plan has root");
            let key = node
                .resource
                .as_ref()
                .expect("instance node has resource key");
            let name = leased_try!(executor::acquire_instance(context, &door, key, &holder));
            instance_leases.push((
                key.clone(),
                super::leases::keep_path(&dir, slot - 1),
                pool.keep
                    .as_deref()
                    .and_then(wt_core::model::duration_millis)
                    .unwrap_or(0),
            ));
            name
        };
        lease_env.insert(env_key, lease_name);
        guards.push(guard);
    }

    let mut request = CommandRequest::new(&args.cmd[0]);
    request.args = args.cmd[1..].iter().map(Into::into).collect();
    request.cwd = Some(Path::new(door.tree.path.as_str()).to_path_buf());
    request.clear_env = true;
    request.env = door.env.env.clone();
    request.env.extend(lease_env.clone());
    let stamp = leased_try!(wt_sys::fsx::timestamp()).replace(':', "-");
    let log = Path::new(door.tree.path.as_str())
        .join(".wt/logs")
        .join(format!("with-{stamp}.log"));
    leased_try!(wt_sys::fsx::create_private_dir(
        log.parent().expect("lease log has a parent")
    ));
    let output = wt_sys::proc::run(
        &request,
        Some(&log),
        None,
        if context.json {
            Tee::Stderr
        } else {
            Tee::Inherit
        },
    );
    finish_instance_leases(context, &door, &instance_leases, true)?;
    drop(guards);
    let output = output?;
    if !output.success() {
        return Err(CoreError::new(
            ExitClass::ChildFailed,
            "WITH_FAILED",
            format!(
                "leased command exited {}",
                output.child.code.map_or_else(
                    || output.child.signal.map_or_else(
                        || "without status".to_owned(),
                        |signal| format!("on signal {signal}")
                    ),
                    |code| code.to_string()
                )
            ),
            "inspect the command output and lease log, then retry",
        )
        .with_details(serde_json::json!({"child": output.child, "log": log})));
    }
    Output::data(serde_json::json!({
        "target": door.target.to_string(),
        "resources": args.resources,
        "ensured": durable,
        "leases": lease_env,
        "child": output.child,
        "log": log,
    }))
}

fn finish_instance_leases(
    context: &Context,
    door: &door::Door,
    leases: &[InstanceLease],
    keep: bool,
) -> Result<(), CoreError> {
    let mut first_error = None;
    for (key, keep_path, keep_millis) in leases {
        let result = if keep && *keep_millis > 0 {
            let until_millis = super::leases::now_millis().saturating_add(*keep_millis);
            super::leases::write_keep(
                keep_path,
                &super::leases::Keep {
                    tree: door.target.to_string(),
                    until_millis,
                    resource: key.clone(),
                },
            )
            .and_then(|()| {
                executor::release_instance(
                    context,
                    door,
                    key,
                    wt_core::resource::ResourceHold::Kept {
                        tree: door.target.to_string(),
                        until_millis,
                    },
                )
            })
        } else {
            super::leases::clear_keep(keep_path).and_then(|()| {
                executor::release_instance(
                    context,
                    door,
                    key,
                    wt_core::resource::ResourceHold::Free,
                )
            })
        };
        if first_error.is_none() {
            first_error = result.err();
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(crate) fn lease_key(name: &str) -> String {
    format!(
        "WT_LEASE_{}",
        name.chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    )
}

pub(crate) fn destroy_idle_declared(
    context: &mut Context,
    door: &door::Door,
) -> Result<(), CoreError> {
    let scope = wt_core::config::effective_scope(&door.config, ".")?;
    let arenas = executor::arena_snapshot(context, &door.target.label)?;
    for (name, pool) in scope.resources.iter().filter(|(_, resource)| {
        resource.count.is_some() && !resource.token_pool() && resource.idle.is_some()
    }) {
        let idle = pool
            .idle
            .as_deref()
            .and_then(wt_core::model::duration_millis)
            .expect("validated idle duration");
        let count = pool.count.unwrap_or(1);
        let dir = lease_dir(context, door, name, pool.tied_to);
        let occupancy = wt_sys::lock::named_occupancy(&dir, count)?;
        for slot in 0..count {
            if occupancy.holders.iter().any(|holder| holder.slot == slot)
                || super::leases::active_keep(&super::leases::keep_path(&dir, slot))?.is_some()
            {
                continue;
            }
            let id = format!("{name}#{}", slot + 1);
            let plan = executor::plan(context, door, &id)?;
            let key = plan
                .nodes
                .iter()
                .find(|node| node.id == id)
                .and_then(|node| node.resource.as_ref())
                .expect("instance pool root has a resource key");
            let holder = context.holder(door.target.to_string(), "idle")?;
            let _resource = wt_sys::lock::resource(
                &executor::resource_lock_path(context, key),
                &holder,
                super::context::duration(
                    context.settings.waits.resource.as_deref(),
                    Duration::from_secs(120),
                ),
            )?;
            if wt_sys::lock::named_occupancy(&dir, count)?
                .holders
                .iter()
                .any(|holder| holder.slot == slot)
                || super::leases::active_keep(&super::leases::keep_path(&dir, slot))?.is_some()
            {
                continue;
            }
            let record = match executor::load_record(context, door, key) {
                Ok(record) => record,
                Err(error) if error.code.0 == "RESOURCE_RECORD_MISSING" => continue,
                Err(error) => return Err(error),
            };
            if record.instance.is_none()
                || !record.released_millis.is_some_and(|released| {
                    super::leases::now_millis().saturating_sub(released) >= idle
                })
            {
                continue;
            }
            executor::destroy_stored_resource(context, &door.target, record, false, &arenas)?;
        }
    }
    Ok(())
}

pub(crate) fn spawn_idle_cleanup(context: &Context) -> Result<(), CoreError> {
    for tree in &context.registry.trees {
        if !matches!(
            wt_sys::fsx::path_kind(Path::new(tree.path.as_str()))?,
            wt_sys::fsx::PathKind::Directory
        ) || !context.identity_ok(tree)?
        {
            continue;
        }
        let config = context.load_config(tree)?;
        let scope = wt_core::config::effective_scope(&config, ".")?;
        if !scope
            .resources
            .values()
            .any(|resource| resource.count.is_some() && resource.idle.is_some())
        {
            continue;
        }
        let target = super::context::target_of(tree).to_string();
        super::build::spawn_wt(
            context,
            Path::new(tree.path.as_str()),
            &["__hook", "idle", &target],
            &[],
            None,
            None,
        )?;
    }
    Ok(())
}

pub(crate) fn lease_dir(
    context: &Context,
    door: &door::Door,
    name: &str,
    tied_to: Option<wt_core::config::TiedTo>,
) -> std::path::PathBuf {
    match tied_to.unwrap_or(wt_core::config::TiedTo::Repo) {
        wt_core::config::TiedTo::Machine => context.home.join(format!("leases/machine/{name}")),
        wt_core::config::TiedTo::Repo => context
            .home
            .join(format!("leases/repo/{}/{name}", door.tree.label)),
        wt_core::config::TiedTo::Tree => context.home.join(format!(
            "leases/tree/{}/{}/{name}",
            door.tree.label, door.tree.name
        )),
    }
}

fn lease_error(name: &str, error: wt_sys::lock::NamedAcquireError) -> CoreError {
    match error {
        wt_sys::lock::NamedAcquireError::Other(error) => error,
        wt_sys::lock::NamedAcquireError::Held(occupancy) => CoreError::new(
            ExitClass::Conflict,
            "LEASE_HELD",
            format!(
                "pool `{name}` has all {}/{} slots leased",
                occupancy.holders.len(),
                occupancy.slots
            ),
            "wait for a holder to finish or raise the pool count",
        )
        .with_details(serde_json::json!({
            "pool": name,
            "slots": occupancy.slots,
            "holders": occupancy.holders.iter().map(|slot| serde_json::json!({
                "slot": slot.slot + 1,
                "holder": slot.holder,
            })).collect::<Vec<_>>()
        })),
    }
}

fn acquire_slot(
    context: &Context,
    door: &door::Door,
    name: &str,
    pool: &wt_core::config::Resource,
    dir: &Path,
    holder: &wt_sys::lock::Holder,
    wait: Option<Duration>,
) -> Result<wt_sys::lock::NamedToken, CoreError> {
    let started = Instant::now();
    let slots = pool.count.unwrap_or(1);
    loop {
        let occupancy = wt_sys::lock::named_occupancy(dir, slots)?;
        let mut ranked = Vec::new();
        let mut reservations = Vec::new();
        for slot in 0..slots {
            let keep_path = super::leases::keep_path(dir, slot);
            let keep = super::leases::active_keep(&keep_path)?;
            if let Some(keep) = &keep {
                if keep.tree == door.target.to_string() {
                    ranked.push((0_u8, slot));
                } else {
                    reservations.push(keep.clone());
                }
                continue;
            }
            if pool.token_pool() {
                ranked.push((0, slot));
                continue;
            }
            let id = format!("{name}#{}", slot + 1);
            let plan = executor::plan(context, door, &id)?;
            let key = plan
                .nodes
                .iter()
                .find(|node| node.id == id)
                .and_then(|node| node.resource.as_ref())
                .expect("instance pool root has a resource key");
            let record = executor::load_record(context, door, key).ok();
            let present = record
                .as_ref()
                .is_some_and(|record| record.instance.is_some());
            // An orphan cannot be created or acquired until someone repairs
            // it, so it is tried only after every other slot: one broken
            // instance must not refuse a lease another slot could serve.
            let orphaned = record
                .as_ref()
                .is_some_and(|record| record.state == wt_core::resource::ResourceState::Orphaned);
            let leased = occupancy.holders.iter().any(|holder| holder.slot == slot);
            ranked.push((
                if orphaned {
                    4
                } else if !present {
                    2
                } else if leased {
                    3
                } else {
                    1
                },
                slot,
            ));
        }
        ranked.sort();
        let order = ranked.into_iter().map(|(_, slot)| slot);
        for slot in order {
            match wt_sys::lock::lease_ordered(dir, slots, &[slot], holder, Some(Duration::ZERO)) {
                Ok(guard) => {
                    let keep_path = super::leases::keep_path(dir, slot);
                    if let Some(keep) = super::leases::active_keep(&keep_path)? {
                        if keep.tree != door.target.to_string() {
                            drop(guard);
                            reservations.push(keep);
                            continue;
                        }
                    }
                    super::leases::clear_keep(&keep_path)?;
                    return Ok(guard);
                }
                Err(wt_sys::lock::NamedAcquireError::Held(_)) => {}
                Err(error) => return Err(lease_error(name, error)),
            }
        }
        if wait.is_some_and(|limit| started.elapsed() >= limit) {
            let occupancy = wt_sys::lock::named_occupancy(dir, slots)?;
            let (waiting_on, tree) = if let Some(keep) = reservations.first() {
                ("reservation", Some(keep.tree.clone()))
            } else {
                ("work", None)
            };
            return Err(CoreError::new(
                ExitClass::Conflict,
                "LEASE_HELD",
                tree.as_ref().map_or_else(
                    || format!("pool `{name}` remained held by running work"),
                    |tree| format!("pool `{name}` remained reserved for tree `{tree}`"),
                ),
                "wait for the hold to end or increase the pool wait",
            )
            .with_details(serde_json::json!({
                "pool": name,
                "waiting_on": waiting_on,
                "tree": tree,
                "holders": occupancy.holders.iter().map(|slot| serde_json::json!({
                    "slot": slot.slot + 1,
                    "holder": slot.holder,
                })).collect::<Vec<_>>(),
            })));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
