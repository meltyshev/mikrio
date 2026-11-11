//! Checked host representation conversions; semantic validation stays in Rust.

use pyo3::exceptions::{PyOverflowError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyInt, PyMapping, PyString};

pub(crate) fn string(value: &Bound<'_, PyString>) -> PyResult<String> {
    value.to_str().map(str::to_owned).map_err(|_| {
        crate::storage_error(
            value.py(),
            "InvalidArgument",
            "string must contain Unicode scalar values",
        )
    })
}

pub(crate) fn integer(value: &Bound<'_, PyAny>) -> PyResult<u64> {
    if value.is_instance_of::<PyBool>() || !value.is_instance_of::<PyInt>() {
        return Err(PyTypeError::new_err("expected an integer, excluding bool"));
    }

    value.extract().map_err(|_| {
        crate::storage_error(
            value.py(),
            "InvalidArgument",
            "integer is outside the supported range",
        )
    })
}

pub(crate) fn bound(value: &Bound<'_, PyAny>) -> PyResult<usize> {
    usize::try_from(integer(value)?).map_err(|_| {
        crate::storage_error(
            value.py(),
            "InvalidArgument",
            "byte bound is outside the host range",
        )
    })
}

pub(crate) fn timestamp(value: &Bound<'_, PyAny>) -> PyResult<mikrio::Timestamp> {
    let py = value.py();
    let module = py.import("datetime")?;
    let datetime = module.getattr("datetime")?;

    if !value.is_instance(&datetime)? {
        return Err(PyTypeError::new_err("expected a timezone-aware datetime"));
    }

    if value.call_method0("utcoffset")?.is_none() {
        return Err(crate::storage_error(
            py,
            "InvalidArgument",
            "datetime must be timezone-aware",
        ));
    }

    let utc = module.getattr("UTC")?;
    let epoch = datetime.call1((1970, 1, 1, 0, 0, 0, 0, &utc))?;

    let normalized = value.call_method1("astimezone", (&utc,)).map_err(|cause| {
        if !cause.is_instance_of::<PyOverflowError>(py) {
            return cause;
        }

        let error = crate::storage_error(
            py,
            "InvalidArgument",
            "datetime is outside the supported UTC range",
        );

        error.set_cause(py, Some(cause));
        error
    })?;

    let delta = normalized.call_method1("__sub__", (epoch,))?;
    let days: i64 = delta.getattr("days")?.extract()?;
    let seconds: i64 = delta.getattr("seconds")?.extract()?;
    let micros: i64 = delta.getattr("microseconds")?.extract()?;

    let millis = days
        .checked_mul(86_400_000)
        .and_then(|days_ms| {
            seconds
                .checked_mul(1_000)
                .and_then(|seconds_ms| days_ms.checked_add(seconds_ms))
        })
        .and_then(|whole_seconds_ms| whole_seconds_ms.checked_add(micros / 1_000))
        .ok_or_else(|| {
            crate::storage_error(
                py,
                "InvalidArgument",
                "datetime is outside the supported range",
            )
        })?;

    mikrio::Timestamp::from_unix_millis(millis).map_err(|error| crate::native_error(py, &error))
}

pub(crate) fn datetime(py: Python<'_>, value: mikrio::Timestamp) -> PyResult<Py<PyAny>> {
    let module = py.import("datetime")?;
    let utc = module.getattr("UTC")?;

    let epoch = module
        .getattr("datetime")?
        .call1((1970, 1, 1, 0, 0, 0, 0, utc))?;

    let millis = value.as_unix_millis();

    // Integral days/seconds/microseconds avoid float rounding across year 1-9999.
    let delta = module.getattr("timedelta")?.call1((
        millis.div_euclid(86_400_000),
        millis.rem_euclid(86_400_000) / 1_000,
        millis.rem_euclid(1_000) * 1_000,
    ))?;

    Ok(epoch.call_method1("__add__", (delta,))?.unbind())
}

pub(crate) fn metadata(value: &Bound<'_, PyAny>) -> PyResult<Vec<(String, String)>> {
    let mapping = value.cast::<PyMapping>()?;
    let mut entries = Vec::new();

    for key in value.try_iter()? {
        // Bound conversion before allocating entries from an arbitrary mapping.
        if entries.len() == 64 {
            return Err(crate::storage_error(
                value.py(),
                "InvalidArgument",
                "metadata exceeds its entry limit",
            ));
        }

        let key = key?;
        let entry = mapping.get_item(&key)?;

        entries.push((
            string(key.cast::<PyString>()?)?,
            string(entry.cast::<PyString>()?)?,
        ));
    }

    Ok(entries)
}

pub(crate) fn checksum_algorithm(
    value: &Bound<'_, PyString>,
) -> PyResult<mikrio::ChecksumAlgorithm> {
    match string(value)?.as_str() {
        "CRC32" => Ok(mikrio::ChecksumAlgorithm::Crc32),
        "CRC32C" => Ok(mikrio::ChecksumAlgorithm::Crc32c),
        "CRC64NVME" => Ok(mikrio::ChecksumAlgorithm::Crc64Nvme),
        "SHA1" => Ok(mikrio::ChecksumAlgorithm::Sha1),
        "SHA256" => Ok(mikrio::ChecksumAlgorithm::Sha256),
        _ => Err(crate::storage_error(
            value.py(),
            "InvalidArgument",
            "unsupported checksum algorithm",
        )),
    }
}

pub(crate) fn checksum_mode(value: &Bound<'_, PyString>) -> PyResult<mikrio::ChecksumMode> {
    if string(value)? != "ENABLED" {
        return Err(crate::storage_error(
            value.py(),
            "InvalidArgument",
            "unsupported checksum mode",
        ));
    }

    Ok(mikrio::ChecksumMode::Enabled)
}

pub(crate) fn tags(value: &Bound<'_, PyAny>) -> PyResult<Vec<mikrio::Tag>> {
    let mut tags = Vec::new();

    for entry in value.try_iter()? {
        if tags.len() == 10 {
            return Err(crate::storage_error(
                value.py(),
                "InvalidArgument",
                "at most ten tags are accepted",
            ));
        }

        let entry = entry?;
        let tag: PyRef<'_, crate::outputs::Tag> = entry.extract()?;

        tags.push(mikrio::Tag {
            key: tag.key.clone(),
            value: tag.value.clone(),
        });
    }

    Ok(tags)
}
