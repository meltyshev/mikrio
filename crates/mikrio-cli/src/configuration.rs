//! Select command-scoped configuration and construct the facade's checked inputs.

use std::collections::HashSet;
use std::env;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::ArgMatches;
use mikrio::{
    CreateAccessKeyInput, CreateBucketInput, CreatePrincipalInput, Credentials,
    DeleteAccessKeyInput, DeleteBucketPolicyInput, DeletePrincipalInput,
    DeletePrincipalPolicyInput, GetBucketPolicyInput, GetPrincipalInput, GetPrincipalPolicyInput,
    ListAccessKeysInput, ListPrincipalsInput, ListenOptions, OpenOptions, PutBucketPolicyInput,
    PutPrincipalPolicyInput, SetPrincipalEnabledInput,
};

use crate::arguments::Arguments;
use crate::failure::{Failure, Phase};

pub(crate) struct Request {
    pub(crate) open: OpenOptions,
    pub(crate) buckets: Vec<CreateBucketInput>,
    pub(crate) operation: Operation,
}

pub(crate) enum Operation {
    Init,
    Serve {
        options: ListenOptions,
        generated_credentials: bool,
    },
    CreatePrincipal(CreatePrincipalInput),
    GetPrincipal(GetPrincipalInput),
    ListPrincipals(ListPrincipalsInput),
    SetPrincipalEnabled(SetPrincipalEnabledInput),
    DeletePrincipal(DeletePrincipalInput),
    CreateAccessKey(CreateAccessKeyInput),
    ListAccessKeys(ListAccessKeysInput),
    RevokeAccessKey(DeleteAccessKeyInput),
    ApplyBucketPolicy(PutBucketPolicyInput),
    GetBucketPolicy(GetBucketPolicyInput),
    DeleteBucketPolicy(DeleteBucketPolicyInput),
    ApplyPrincipalPolicy(PutPrincipalPolicyInput),
    GetPrincipalPolicy(GetPrincipalPolicyInput),
    DeletePrincipalPolicy(DeletePrincipalPolicyInput),
}

impl Operation {
    pub(crate) fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::CreatePrincipal(_)
                | Self::SetPrincipalEnabled(_)
                | Self::DeletePrincipal(_)
                | Self::CreateAccessKey(_)
                | Self::RevokeAccessKey(_)
                | Self::ApplyBucketPolicy(_)
                | Self::DeleteBucketPolicy(_)
                | Self::ApplyPrincipalPolicy(_)
                | Self::DeletePrincipalPolicy(_)
        )
    }
}

pub(crate) async fn prepare(
    arguments: &Arguments,
    cancellation: Arc<AtomicBool>,
) -> Result<Request, Failure> {
    let values = &arguments.values;
    let creates = matches!(arguments.command, "init" | "serve");
    let mut open = OpenOptions::new(required(values, "directory")?).create_if_missing(creates);
    let mut buckets = Vec::new();

    if creates {
        if let Some(region) = selected(values, "region", "mikrio_region")? {
            open = open.region(region);
        }

        let mut seen = HashSet::new();

        for bucket in values.get_many::<String>("bucket").into_iter().flatten() {
            let input = CreateBucketInput::builder(bucket)
                .build()
                .map_err(argument_storage)?;

            if seen.insert(bucket) {
                buckets.push(input);
            }
        }
    }

    open.validate().map_err(argument_storage)?;

    let operation = match arguments.command {
        "init" => Operation::Init,
        "serve" => listener(values)?,
        "principal create" => Operation::CreatePrincipal(
            CreatePrincipalInput::builder(required(values, "name")?)
                .build()
                .map_err(argument_storage)?,
        ),
        "principal get" => Operation::GetPrincipal(
            GetPrincipalInput::builder(required(values, "principal-id")?)
                .build()
                .map_err(argument_storage)?,
        ),
        "principal list" => Operation::ListPrincipals(principals_page(values)?),
        "principal enable" | "principal disable" => Operation::SetPrincipalEnabled(
            SetPrincipalEnabledInput::builder(
                required(values, "principal-id")?,
                arguments.command == "principal enable",
            )
            .build()
            .map_err(argument_storage)?,
        ),
        "principal delete" => Operation::DeletePrincipal(
            DeletePrincipalInput::builder(required(values, "principal-id")?)
                .build()
                .map_err(argument_storage)?,
        ),
        "access-key create" => Operation::CreateAccessKey(
            CreateAccessKeyInput::builder(required(values, "principal-id")?)
                .build()
                .map_err(argument_storage)?,
        ),
        "access-key list" => Operation::ListAccessKeys(keys_page(values)?),
        "access-key revoke" => Operation::RevokeAccessKey(
            DeleteAccessKeyInput::builder(
                required(values, "principal-id")?,
                required(values, "access-key-id")?,
            )
            .build()
            .map_err(argument_storage)?,
        ),
        "policy apply" | "policy get" | "policy delete" => {
            policy(arguments.command, values, cancellation).await?
        }
        _ => return Err(Failure::argument("unrecognized command")),
    };

    Ok(Request {
        open,
        buckets,
        operation,
    })
}

fn required<'a>(values: &'a ArgMatches, name: &str) -> Result<&'a str, Failure> {
    values
        .get_one::<String>(name)
        .map(String::as_str)
        .ok_or_else(|| Failure::argument(format!("missing required {name}")))
}

fn selected(values: &ArgMatches, option: &str, variable: &str) -> Result<Option<String>, Failure> {
    if let Some(value) = values.get_one::<String>(option) {
        return Ok(Some(value.clone()));
    }

    environment(variable)
}

fn environment(name: &str) -> Result<Option<String>, Failure> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err(Failure::argument(format!("{name} must be UTF-8")))
        }
    }
}

fn listener(values: &ArgMatches) -> Result<Operation, Failure> {
    let mut options = ListenOptions::default();

    if let Some(host) = selected(values, "host", "mikrio_host")? {
        options = options.host(host);
    }

    if let Some(port) = selected(values, "port", "mikrio_port")? {
        options = options.port(decimal(
            &port,
            "port must be an integer from 0 through 65535",
        )?);
    }

    if let Some(origin) = selected(values, "public-endpoint", "mikrio_public_endpoint")? {
        options = options.public_endpoint(origin);
    }

    let credentials = match (
        environment("mikrio_access_key_id")?,
        environment("mikrio_secret_access_key")?,
    ) {
        (None, None) => None,
        (Some(access_key_id), Some(secret_access_key)) => {
            Some(Credentials::new(access_key_id, secret_access_key).map_err(argument_storage)?)
        }
        _ => {
            return Err(Failure::argument(
                "mikrio_access_key_id and mikrio_secret_access_key must be supplied together",
            ));
        }
    };

    let generated_credentials = credentials.is_none();

    if let Some(credentials) = credentials {
        options = options.credentials(credentials);
    }

    options.validate().map_err(argument_storage)?;

    Ok(Operation::Serve {
        options,
        generated_credentials,
    })
}

fn decimal<T: std::str::FromStr>(value: &str, message: &'static str) -> Result<T, Failure> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Failure::argument(message));
    }

    value.parse().map_err(|_| Failure::argument(message))
}

fn principals_page(values: &ArgMatches) -> Result<ListPrincipalsInput, Failure> {
    let mut builder = ListPrincipalsInput::builder();

    if let Some(size) = values.get_one::<String>("max-results") {
        builder = builder.max_results(decimal(
            size,
            "max-results must be an integer from 1 through 1000",
        )?);
    }

    if let Some(token) = values.get_one::<String>("continuation-token") {
        builder = builder.continuation_token(token);
    }

    builder.build().map_err(argument_storage)
}

fn keys_page(values: &ArgMatches) -> Result<ListAccessKeysInput, Failure> {
    let mut builder = ListAccessKeysInput::builder(required(values, "principal-id")?);

    if let Some(size) = values.get_one::<String>("max-results") {
        builder = builder.max_results(decimal(
            size,
            "max-results must be an integer from 1 through 1000",
        )?);
    }

    if let Some(token) = values.get_one::<String>("continuation-token") {
        builder = builder.continuation_token(token);
    }

    builder.build().map_err(argument_storage)
}

async fn policy(
    command: &str,
    values: &ArgMatches,
    cancellation: Arc<AtomicBool>,
) -> Result<Operation, Failure> {
    // Validate target envelopes before consuming a potentially blocking input.
    if let Some(bucket) = values.get_one::<String>("bucket") {
        let target = GetBucketPolicyInput::builder(bucket)
            .build()
            .map_err(argument_storage)?;

        return match command {
            "policy get" => Ok(Operation::GetBucketPolicy(target)),
            "policy delete" => Ok(Operation::DeleteBucketPolicy(
                DeleteBucketPolicyInput::builder(bucket)
                    .build()
                    .map_err(argument_storage)?,
            )),
            _ => Ok(Operation::ApplyBucketPolicy(
                PutBucketPolicyInput::builder(
                    bucket,
                    crate::input::read_policy(required(values, "file")?, cancellation).await?,
                )
                .build()
                .map_err(input_storage)?,
            )),
        };
    }

    let principal_id = required(values, "principal-id")?;

    let target = GetPrincipalPolicyInput::builder(principal_id)
        .build()
        .map_err(argument_storage)?;

    match command {
        "policy get" => Ok(Operation::GetPrincipalPolicy(target)),
        "policy delete" => Ok(Operation::DeletePrincipalPolicy(
            DeletePrincipalPolicyInput::builder(principal_id)
                .build()
                .map_err(argument_storage)?,
        )),
        _ => Ok(Operation::ApplyPrincipalPolicy(
            PutPrincipalPolicyInput::builder(
                principal_id,
                crate::input::read_policy(required(values, "file")?, cancellation).await?,
            )
            .build()
            .map_err(input_storage)?,
        )),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Result::map_err supplies an owned storage failure"
)]
fn argument_storage(error: mikrio::StorageError) -> Failure {
    Failure::storage(Phase::Arguments, &error, false)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Result::map_err supplies an owned storage failure"
)]
fn input_storage(error: mikrio::StorageError) -> Failure {
    Failure::storage(Phase::Input, &error, true)
}
