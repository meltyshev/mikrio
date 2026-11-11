//! Public multipart operations share the core upload state machine.

use crate::{ObjectRef, StorageError};

mod completion;
mod copy;
mod create;
mod listing;
mod part;

pub use completion::*;
pub use copy::*;
pub use create::*;
pub use listing::*;
pub use part::*;

#[derive(Debug, Clone)]
struct Upload {
    object: ObjectRef,
    token: String,
    id: mikrio_core::UploadId,
    owner: Option<String>,
}

impl Upload {
    fn new(
        bucket: &str,
        key: &str,
        token: String,
        owner: Option<String>,
    ) -> Result<Self, StorageError> {
        let object = ObjectRef::new(bucket, key)?;
        let upload_id = mikrio_core::UploadId::decode(&token).map_err(StorageError::from_value)?;
        validate_owner(owner.as_deref())?;

        Ok(Self {
            object,
            token,
            id: upload_id,
            owner,
        })
    }

    fn into_core(self) -> mikrio_core::multipart::UploadRequest {
        let (bucket, key) = self.object.into_core();

        mikrio_core::multipart::UploadRequest {
            bucket,
            key,
            upload_id: self.id,
            expected_owner: self.owner,
        }
    }
}

fn validate_owner(value: Option<&str>) -> Result<(), StorageError> {
    if let Some(value) = value {
        mikrio_core::access::validate_account_id(value).map_err(StorageError::from_identity)?;
    }

    Ok(())
}
