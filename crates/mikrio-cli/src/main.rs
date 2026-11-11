//! Foreground CLI over the public facade. This executable owns one runtime.

use std::io::{self, Write};
use std::process::ExitCode;

use mikrio::{ListenOptions, Store};

mod arguments;
mod command;
mod configuration;
mod diagnostics;
mod failure;
mod input;
mod presentation;
mod shutdown;

use arguments::{Arguments, Parsed};
use configuration::Operation;
use diagnostics::Diagnostics;
use failure::{Failure, Phase};
use presentation::Reporter;
use shutdown::Shutdown;

fn main() -> ExitCode {
    let command_line = std::env::args_os().collect::<Vec<_>>();

    let arguments = match arguments::parse(&command_line) {
        Ok(Parsed::Display(text)) => {
            let result = io::stdout()
                .lock()
                .write_all(text.as_bytes())
                .and_then(|()| io::stdout().flush());

            if result.is_ok() {
                return ExitCode::SUCCESS;
            }

            let mut reporter = Reporter::new(io::stdout(), io::stderr(), false, None);
            reporter.error(&Failure::io(
                Phase::Output,
                "cannot write help or version output",
            ));

            return ExitCode::from(reporter.exit());
        }
        Ok(Parsed::Command(arguments)) => arguments,
        Err(error) => {
            let mut reporter = Reporter::new(io::stdout(), io::stderr(), error.json, error.command);
            reporter.error(&Failure::argument(error.message));
            return ExitCode::from(reporter.exit());
        }
    };

    let mut reporter = Reporter::new(
        io::stdout(),
        io::stderr(),
        arguments.json,
        Some(arguments.command),
    );

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        reporter.error(&Failure::io(
            Phase::Open,
            "cannot create the command runtime",
        ));

        return ExitCode::from(reporter.exit());
    };

    let (mut diagnostics, dispatch) = Diagnostics::new();

    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(run(arguments, &mut reporter, &mut diagnostics));
    });

    finish_diagnostics(&mut diagnostics, &mut reporter, false);
    // Explicit close attempts and owned input tasks settle before destruction.
    drop(runtime);
    ExitCode::from(reporter.exit())
}

async fn run(
    arguments: Arguments,
    reporter: &mut Reporter<impl Write, impl Write>,
    diagnostics: &mut Diagnostics,
) {
    let Ok(mut shutdown) = Shutdown::register() else {
        reporter.error(&Failure::io(
            Phase::Open,
            "cannot register shutdown signals",
        ));

        return;
    };

    let cancellation = shutdown.cancellation();
    let request = match shutdown
        .settle(configuration::prepare(&arguments, cancellation))
        .await
    {
        Ok(request) => request,
        Err(error) => {
            reporter.error(&error);
            return;
        }
    };

    if shutdown.is_requested().await {
        reporter.error(&Failure::interrupted(Phase::Input));
        return;
    }

    let store = match shutdown.settle(Store::open(request.open)).await {
        Ok(store) => store,
        Err(error) => {
            reporter.error(&Failure::storage(Phase::Open, &error, false));
            return;
        }
    };

    if !bootstrap(&store, request.buckets, &mut shutdown, reporter).await {
        close(&store, reporter, diagnostics, false).await;
        return;
    }

    if let Operation::Serve {
        options,
        generated_credentials,
    } = request.operation
    {
        serve(
            &store,
            options,
            generated_credentials,
            &mut shutdown,
            reporter,
            diagnostics,
        )
        .await;

        close(&store, reporter, diagnostics, false).await;
        return;
    }

    let mutation = request.operation.is_mutation();
    let result = shutdown
        .settle(command::execute(&store, request.operation))
        .await;

    let completed_mutation = mutation && result.is_ok();

    match result {
        Ok(result) => {
            // Disclose a one-time key before close, even if shutdown raced commit.
            if reporter.result(&result).is_err() {
                reporter.error(
                    &Failure::io(Phase::Output, "cannot write command result").completed(mutation),
                );
            }

            if shutdown.is_requested().await {
                reporter.error(&Failure::interrupted(Phase::Operation).completed(mutation));
            }
        }
        Err(error) => reporter.error(&error),
    }

    close(&store, reporter, diagnostics, completed_mutation).await;
}

async fn bootstrap(
    store: &Store,
    buckets: Vec<mikrio::CreateBucketInput>,
    shutdown: &mut Shutdown,
    reporter: &mut Reporter<impl Write, impl Write>,
) -> bool {
    for bucket in buckets {
        if shutdown.is_requested().await {
            reporter.error(&Failure::interrupted(Phase::Operation));
            return false;
        }

        if let Err(error) = shutdown.settle(store.create_bucket(bucket)).await {
            if error.code() == mikrio::ErrorCode::BucketAlreadyOwnedByYou {
                continue;
            }

            // This is a multi-step setup, not a whole-command transaction. Keep
            // the engine's per-operation outcome without inventing a batch one.
            let mut failure = Failure::storage(Phase::Operation, &error, false);
            failure.message.push_str(
                "; this outcome describes the failed bucket operation; earlier setup may have committed",
            );

            reporter.error(&failure);
            return false;
        }
    }

    if shutdown.is_requested().await {
        reporter.error(&Failure::interrupted(Phase::Open));
        return false;
    }

    true
}

async fn serve(
    store: &Store,
    options: ListenOptions,
    generated_credentials: bool,
    shutdown: &mut Shutdown,
    reporter: &mut Reporter<impl Write, impl Write>,
    diagnostics: &mut Diagnostics,
) {
    let server = match shutdown.settle(store.listen(options)).await {
        Ok(server) => server,
        Err(error) => {
            reporter.error(&Failure::storage(Phase::Open, &error, false));
            return;
        }
    };

    if shutdown.is_requested().await {
        reporter.error(&Failure::interrupted(Phase::Open));
    } else if reporter.ready(&server, generated_credentials).is_err() {
        reporter.error(&Failure::io(Phase::Output, "cannot write readiness output"));
    } else {
        loop {
            tokio::select! {
                () = shutdown.wait() => break,
                result = server.closed() => {
                    match result {
                        Ok(()) => reporter.error(&Failure::io(Phase::Operation, "listener stopped without a shutdown request")),
                        Err(error) => reporter.error(&Failure::storage(Phase::Operation, &error, false)),
                    }

                    break;
                }
                Some(record) = diagnostics.receive() => {
                    if reporter.diagnostic(&record).is_err() {
                        reporter.error(&Failure::io(Phase::Output, "cannot write diagnostic output"));
                        break;
                    }
                }
            }
        }
    }

    if let Err(error) = server.close().await {
        reporter.error(&Failure::storage(Phase::Close, &error, false));
    }
}

async fn close(
    store: &Store,
    reporter: &mut Reporter<impl Write, impl Write>,
    diagnostics: &mut Diagnostics,
    completed_mutation: bool,
) {
    if let Err(error) = store.close().await {
        reporter
            .error(&Failure::storage(Phase::Close, &error, false).completed(completed_mutation));
    }

    finish_diagnostics(diagnostics, reporter, completed_mutation);
}

fn finish_diagnostics(
    diagnostics: &mut Diagnostics,
    reporter: &mut Reporter<impl Write, impl Write>,
    completed_mutation: bool,
) {
    diagnostics.close();

    while let Some(record) = diagnostics.pending() {
        if reporter.diagnostic(&record).is_err() {
            reporter.error(
                &Failure::io(Phase::Output, "cannot write diagnostic output")
                    .completed(completed_mutation),
            );

            while diagnostics.pending().is_some() {}

            break;
        }
    }
}
