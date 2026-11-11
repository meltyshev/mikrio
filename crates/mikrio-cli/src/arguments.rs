//! Command grammar only. Parsing never reads configuration or opens resources.

use std::ffi::OsString;

use clap::error::{ContextKind, ErrorKind};
use clap::{Arg, ArgAction, ArgGroup, ArgMatches, Command};

pub(crate) enum Parsed {
    Display(String),
    Command(Arguments),
}

pub(crate) struct Arguments {
    pub(crate) json: bool,
    pub(crate) command: &'static str,
    pub(crate) values: ArgMatches,
}

pub(crate) struct Rejected {
    pub(crate) json: bool,
    pub(crate) command: Option<&'static str>,
    pub(crate) message: String,
}

pub(crate) fn parse(arguments: &[OsString]) -> Result<Parsed, Rejected> {
    let json_count = arguments
        .iter()
        .skip(1)
        .take_while(|value| *value != "--")
        .filter(|value| *value == "--json")
        .count();

    let command = command_path(arguments);

    if json_count > 1 {
        return Err(Rejected {
            json: true,
            command,
            message: "--json may be supplied only once".into(),
        });
    }

    // clap's generated help navigator treats every following token as a
    // command name. The inherited JSON switch still applies there, while a
    // token after -- remains literal. Normal option values stay with clap.
    let mut path = arguments.iter().skip(1).filter(|word| *word != "--json");

    let help_navigation = match path.next().and_then(|word| word.to_str()) {
        Some("help") => true,
        Some("principal" | "access-key" | "policy") => {
            path.next().is_some_and(|word| word == "help")
        }
        _ => false,
    };

    let mut options = true;

    let values = arguments.iter().skip(1).filter(|word| {
        if *word == "--" {
            options = false;
        }

        !help_navigation || !options || *word != "--json"
    });

    let mut matches =
        match grammar().try_get_matches_from(arguments.first().into_iter().chain(values)) {
            Ok(matches) => matches,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
                ) =>
            {
                return Ok(Parsed::Display(error.to_string()));
            }
            Err(error) => {
                return Err(Rejected {
                    json: json_count != 0,
                    command,
                    message: rejection_message(&error),
                });
            }
        };

    let Some(command) = command else {
        return Err(Rejected {
            json: json_count != 0,
            command: None,
            message: "a command is required; use mikrio --help".into(),
        });
    };

    while let Some((_, child)) = matches.remove_subcommand() {
        matches = child;
    }

    Ok(Parsed::Command(Arguments {
        json: json_count != 0,
        command,
        values: matches,
    }))
}

fn rejection_message(error: &clap::Error) -> String {
    // Parser descriptions can echo private argument values. Usage contains only
    // grammar-defined names, including the explicitly fixed executable name.
    let description = error.kind().as_str().unwrap_or("invalid command arguments");

    let usage = error
        .get(ContextKind::Usage)
        .map_or_else(|| grammar().render_usage().to_string(), ToString::to_string);

    format!("{description}\n\n{usage}\n\nFor more information, try '--help'.")
}

fn command_path(arguments: &[OsString]) -> Option<&'static str> {
    let mut words = arguments.iter().skip(1).filter(|word| *word != "--json");
    let group = words.next()?.to_str()?;

    match group {
        "init" => Some("init"),
        "serve" => Some("serve"),
        "principal" | "access-key" | "policy" => match (group, words.next()?.to_str()?) {
            ("principal", "create") => Some("principal create"),
            ("principal", "get") => Some("principal get"),
            ("principal", "list") => Some("principal list"),
            ("principal", "enable") => Some("principal enable"),
            ("principal", "disable") => Some("principal disable"),
            ("principal", "delete") => Some("principal delete"),
            ("access-key", "create") => Some("access-key create"),
            ("access-key", "list") => Some("access-key list"),
            ("access-key", "revoke") => Some("access-key revoke"),
            ("policy", "apply") => Some("policy apply"),
            ("policy", "get") => Some("policy get"),
            ("policy", "delete") => Some("policy delete"),
            _ => None,
        },
        _ => None,
    }
}

fn grammar() -> Command {
    Command::new("mikrio")
        .bin_name("mikrio")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Local S3 storage and trusted directory administration")
        .subcommand_required(true)
        .arg(
            Arg::new("json")
                .long("json")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Emit schema 1 JSON results and diagnostics"),
        )
        .subcommand(initialization("init", "Initialize or reopen storage and ensure buckets exist"))
        .subcommand(
            initialization("serve", "Serve a foreground S3 listener until shutdown")
                .arg(option("host", "IP", "Bind IP (mikrio_host; default 127.0.0.1)"))
                .arg(option("port", "PORT", "Bind port, 0-65535 (mikrio_port; default 0)"))
                .arg(option("public-endpoint", "ORIGIN", "HTTP(S) response origin (mikrio_public_endpoint)"))
                .after_help("Credentials: set both mikrio_access_key_id and mikrio_secret_access_key, or omit both to generate temporary owner credentials. AWS settings are ignored. Generated readiness credentials are sensitive. SIGINT/SIGTERM (Windows Ctrl+C) close the listener and store."),
        )
        .subcommand(
            Command::new("principal")
                .about("Administer persistent application identities in an existing store")
                .subcommand_required(true)
                .subcommand(leaf("create", "Create an enabled principal with no policy or keys").arg(option("name", "NAME", "Unique application identity name").required(true)))
                .subcommand(principal_target("get", "Read one principal"))
                .subcommand(pagination(leaf("list", "Read one bounded principal page")))
                .subcommand(principal_target("enable", "Enable the principal's current credentials"))
                .subcommand(principal_target("disable", "Disable authentication; preserve policy and keys"))
                .subcommand(principal_target("delete", "Delete a principal after explicitly revoking all its keys")),
        )
        .subcommand(
            Command::new("access-key")
                .about("Create, inventory and revoke persistent application credentials")
                .subcommand_required(true)
                .subcommand(principal_target("create", "Create a key and disclose its secret once").after_help("Store this command's sensitive result securely. Rotation: create and test a second key, then explicitly revoke the old key. Inventory cannot recover a lost secret."))
                .subcommand(pagination(principal_target("list", "Read one key page without secrets")))
                .subcommand(principal_target("revoke", "Revoke only the named principal's key").arg(option("access-key-id", "ID", "Persistent access key ID").required(true))),
        )
        .subcommand(
            Command::new("policy")
                .about("Replace, read or delete one bucket or identity policy locally")
                .subcommand_required(true)
                .subcommand(policy_target("apply", "Atomically replace one complete policy")
                    .arg(option("file", "PATH", "UTF-8 policy file; - explicitly reads stdin (maximum 20 KiB)").required(true))
                    .after_help("Use --file=PATH for a leading-hyphen filename, or --file ./- for a literal file named -. Validation preserves the previous policy on rejection. Block Public Access remains enforced."))
                .subcommand(policy_target("get", "Read the exact stored policy document"))
                .subcommand(policy_target("delete", "Delete a policy; an absent policy on an existing target succeeds")),
        )
        .after_help("All commands require a positional store directory after the complete command path. Administration requires an existing store. Directory access is trusted authority; no HTTP administration or AWS profile discovery. Use -- before a leading-hyphen directory. Exit codes: 0 success, 2 invalid input/configuration, 1 storage/runtime/I/O failure. JSON result output precedes close; also check the exit status.")
}

fn leaf(name: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).arg(
        Arg::new("directory")
            .value_name("DIR")
            .required(true)
            .help("Managed store directory"),
    )
}

fn option(name: &'static str, value: &'static str, help: &'static str) -> Arg {
    Arg::new(name).long(name).value_name(value).help(help)
}

fn initialization(name: &'static str, about: &'static str) -> Command {
    leaf(name, about)
        .arg(option(
            "region",
            "REGION",
            "Signing region (mikrio_region; persisted value or us-east-1)",
        ))
        .arg(
            option(
                "bucket",
                "NAME",
                "Ensure this bucket exists; repeatable, preserves existing configuration",
            )
            .action(ArgAction::Append),
        )
}

fn principal_target(name: &'static str, about: &'static str) -> Command {
    leaf(name, about).arg(
        option(
            "principal-id",
            "ID",
            "Stable principal ID, not a name or ARN",
        )
        .required(true),
    )
}

fn pagination(command: Command) -> Command {
    command
        .arg(option(
            "max-results",
            "N",
            "Page size, 1-1000 (default 100)",
        ))
        .arg(option(
            "continuation-token",
            "TOKEN",
            "Opaque next-page token from the preceding result",
        ))
}

fn policy_target(name: &'static str, about: &'static str) -> Command {
    leaf(name, about)
        .arg(option("bucket", "NAME", "Bucket policy target"))
        .arg(option("principal-id", "ID", "Identity policy target"))
        .group(
            ArgGroup::new("target")
                .args(["bucket", "principal-id"])
                .required(true),
        )
}
