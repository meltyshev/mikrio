//! Facade dispatch and detached results; no policy or persistence engine.

use mikrio::{
    CreateAccessKeyOutput, ListAccessKeysOutput, ListPrincipalsOutput, OwnerInfo, PrincipalInfo,
    Store, StoreInfo,
};

use crate::configuration::Operation;
use crate::failure::{Failure, Phase};

pub(crate) enum ResultValue {
    Init {
        store: StoreInfo,
        owner: OwnerInfo,
    },
    Principal {
        principal: PrincipalInfo,
        account_id: String,
    },
    Principals {
        page: ListPrincipalsOutput,
        account_id: String,
    },
    AccessKey(CreateAccessKeyOutput),
    AccessKeys(ListAccessKeysOutput),
    Policy(String),
    Empty,
}

pub(crate) async fn execute(store: &Store, operation: Operation) -> Result<ResultValue, Failure> {
    match operation {
        Operation::Init => Ok(ResultValue::Init {
            store: store.info().clone(),
            owner: store.get_owner_info().await?,
        }),
        Operation::CreatePrincipal(input) => {
            // Obtain nonsecret presentation context before committing a creation.
            let owner = store.get_owner_info().await?;
            let principal = store.create_principal(input).await?;

            Ok(ResultValue::Principal {
                principal,
                account_id: owner.account_id().into(),
            })
        }
        Operation::GetPrincipal(input) => {
            let owner = store.get_owner_info().await?;
            let principal = store.get_principal(input).await?;

            Ok(ResultValue::Principal {
                principal,
                account_id: owner.account_id().into(),
            })
        }
        Operation::ListPrincipals(input) => {
            let owner = store.get_owner_info().await?;
            let page = store.list_principals(input).await?;

            Ok(ResultValue::Principals {
                page,
                account_id: owner.account_id().into(),
            })
        }
        Operation::SetPrincipalEnabled(input) => {
            let owner = store.get_owner_info().await?;
            let principal = store.set_principal_enabled(input).await?;

            Ok(ResultValue::Principal {
                principal,
                account_id: owner.account_id().into(),
            })
        }
        Operation::DeletePrincipal(input) => {
            store.delete_principal(input).await?;
            Ok(ResultValue::Empty)
        }
        Operation::CreateAccessKey(input) => Ok(ResultValue::AccessKey(
            store.create_access_key(input).await?,
        )),
        Operation::ListAccessKeys(input) => Ok(ResultValue::AccessKeys(
            store.list_access_keys(input).await?,
        )),
        Operation::RevokeAccessKey(input) => {
            store.delete_access_key(input).await?;
            Ok(ResultValue::Empty)
        }
        Operation::ApplyBucketPolicy(input) => {
            store
                .put_bucket_policy(input)
                .await
                .map_err(|error| Failure::storage(Phase::Operation, &error, true))?;

            Ok(ResultValue::Empty)
        }
        Operation::GetBucketPolicy(input) => Ok(ResultValue::Policy(
            store.get_bucket_policy(input).await?.document().into(),
        )),
        Operation::DeleteBucketPolicy(input) => {
            store.delete_bucket_policy(input).await?;
            Ok(ResultValue::Empty)
        }
        Operation::ApplyPrincipalPolicy(input) => {
            store
                .put_principal_policy(input)
                .await
                .map_err(|error| Failure::storage(Phase::Operation, &error, true))?;

            Ok(ResultValue::Empty)
        }
        Operation::GetPrincipalPolicy(input) => Ok(ResultValue::Policy(
            store.get_principal_policy(input).await?.document().into(),
        )),
        Operation::DeletePrincipalPolicy(input) => {
            store.delete_principal_policy(input).await?;
            Ok(ResultValue::Empty)
        }
        // Serving is dispatched by the resource owner, before one-shot execution.
        Operation::Serve { .. } => Err(Failure::io(
            Phase::Operation,
            "listener dispatched as a one-shot command",
        )),
    }
}
