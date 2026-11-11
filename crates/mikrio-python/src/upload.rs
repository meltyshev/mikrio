//! Pull Python producers only on the calling interpreter thread. Large foreign
//! chunks remain host-owned and cross into Rust in demand-driven bounded slices.

use mikrio_embed::Pending;
use mikrio_embed::chunks::CHUNK_BYTES;
use mikrio_embed::upload::Upload;
use pyo3::exceptions::{PyAttributeError, PyException, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyIterator};

use crate::{
    Owner, chain_cleanup_error, host_error, mutation_wait, native_error, outputs, storage_error,
    uncertain_mutation, wait_transfer,
};

enum Producer<'py> {
    Bytes,
    Reader(Bound<'py, PyAny>),
    Iterator(Bound<'py, PyIterator>),
}

struct Source<'py> {
    producer: Producer<'py>,
    chunk: Option<Bound<'py, PyAny>>,
    offset: usize,
}

impl<'py> Source<'py> {
    fn new(body: &Bound<'py, PyAny>) -> PyResult<Self> {
        if body.is_instance_of::<PyBytes>() {
            return Ok(Self {
                producer: Producer::Bytes,
                chunk: Some(byte_view(body)?),
                offset: 0,
            });
        }

        match body.getattr("read") {
            Ok(read) => {
                if !read.is_callable() {
                    return Err(PyTypeError::new_err("body.read must be callable"));
                }

                return Ok(Self {
                    producer: Producer::Reader(read),
                    chunk: None,
                    offset: 0,
                });
            }
            Err(error) if error.is_instance_of::<PyAttributeError>(body.py()) => {}
            Err(error) => return Err(error),
        }

        Ok(Self {
            producer: Producer::Iterator(body.try_iter()?),
            chunk: None,
            offset: 0,
        })
    }

    fn next(
        &mut self,
        py: Python<'py>,
        request: &mikrio_embed::upload::Request,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        loop {
            py.check_signals()?;

            if request.is_canceled() {
                return Ok(None);
            }

            if let Some(chunk) = &self.chunk {
                let length = chunk.len()?;

                if self.offset < length {
                    let end = self.offset.saturating_add(CHUNK_BYTES).min(length);

                    let slice = pyo3::types::PySlice::new(
                        py,
                        isize::try_from(self.offset)?,
                        isize::try_from(end)?,
                        1,
                    );

                    let bytes = chunk
                        .get_item(slice)?
                        .call_method0("tobytes")?
                        .cast_into::<PyBytes>()?;

                    self.offset = end;
                    return Ok(Some(bytes));
                }

                self.chunk = None;
                self.offset = 0;
            }

            let value = match &mut self.producer {
                Producer::Bytes => return Ok(None),
                Producer::Reader(read) => {
                    let value = byte_view(&read.call1((CHUNK_BYTES,))?)?;

                    if value.len()? == 0 {
                        return Ok(None);
                    }

                    value
                }
                Producer::Iterator(iterator) => {
                    let Some(value) = iterator.next() else {
                        return Ok(None);
                    };

                    byte_view(&value?)?
                }
            };

            self.chunk = Some(value);
            // A producer of empty chunks still gives other host threads a turn.
            py.detach(std::thread::yield_now);
        }
    }

    fn finalize(&self) -> PyResult<()> {
        let Producer::Iterator(iterator) = &self.producer else {
            return Ok(());
        };

        match iterator.getattr("close") {
            Ok(close) => {
                close.call0()?;
            }
            Err(error) if error.is_instance_of::<PyAttributeError>(iterator.py()) => {}
            Err(error) => return Err(error),
        }

        Ok(())
    }
}

fn byte_view<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    value
        .py()
        .import("builtins")?
        .getattr("memoryview")?
        .call1((value,))?
        .call_method1("cast", ("B",))
}

pub(crate) fn put<'py>(
    py: Python<'py>,
    owner: &Owner,
    input: mikrio::PutObjectInput,
    upload: Upload,
    body: &Bound<'py, PyAny>,
) -> PyResult<outputs::PutObjectOutput> {
    let store = owner.store.clone();

    execute(
        py,
        owner,
        async move { store.put_object(input).await },
        upload,
        body,
    )
    .map(outputs::PutObjectOutput::from)
}

pub(crate) fn part<'py>(
    py: Python<'py>,
    owner: &Owner,
    input: mikrio::UploadPartInput,
    upload: Upload,
    body: &Bound<'py, PyAny>,
) -> PyResult<crate::multipart::UploadPartOutput> {
    let store = owner.store.clone();

    let output = execute(
        py,
        owner,
        async move { store.upload_part(input).await },
        upload,
        body,
    )?;

    Ok(crate::multipart::UploadPartOutput::new(&output))
}

fn execute<'py, T: Send + 'static>(
    py: Python<'py>,
    owner: &Owner,
    work: impl Future<Output = Result<T, mikrio::StorageError>> + Send + 'static,
    upload: Upload,
    body: &Bound<'py, PyAny>,
) -> PyResult<T> {
    let mut source = Source::new(body)?;

    let pending = owner
        .runtime
        .submit(work)
        .map_err(|error| crate::before_mutation(py, host_error(&error)))?;

    let result = pump(py, owner, pending, upload, &mut source);

    if let Err(primary) = &result
        && let Err(cleanup) = source.finalize()
    {
        chain_cleanup_error(py, primary, cleanup);
    }

    result
}

fn pump<'py, T: Send>(
    py: Python<'py>,
    owner: &Owner,
    pending: Pending<Result<T, mikrio::StorageError>>,
    mut transfer: Upload,
    source: &mut Source<'py>,
) -> PyResult<T> {
    // The admitted upload owns native consumption. Its bounded demand continuation
    // must progress even when every host operation slot belongs to an upload.
    loop {
        let request = wait_transfer(py, &owner.runtime, transfer.next())
            .map_err(|error| uncertain_mutation(py, error))?;

        let Some(request) = request else {
            break;
        };

        match source.next(py, &request) {
            Ok(Some(bytes)) => {
                if request.write(bytes.as_bytes()).is_err() {
                    break;
                }
            }
            Ok(None) => {
                request.finish();
                break;
            }
            Err(cause) => {
                request.fail();

                if !cause.is_instance_of::<PyException>(py) {
                    return Err(cause);
                }

                let error = match mutation_wait(py, pending)? {
                    Err(error) => native_error(py, &error),
                    Ok(_) => storage_error(py, "BodyReadFailed", "host body producer failed"),
                };

                error.set_cause(py, Some(cause));
                return Err(error);
            }
        }
    }

    mutation_wait(py, pending)?.map_err(|error| native_error(py, &error))
}
