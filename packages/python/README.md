# mikrio

Embedded object storage for Python, with an optional S3-compatible HTTP
listener.

```python
from mikrio import Store

with Store.open(directory="./storage") as store:
    store.create_bucket(bucket="photos")
    store.put_object(bucket="photos", key="hello.txt", body=b"Hello, world!")
```

Operations are synchronous and use keyword-only arguments. Use context managers
or explicitly close stores, listeners, and response bodies. The wheel includes
type stubs, `py.typed`, and the matching SQLite companion; keep the package
intact.

See the [project documentation](https://github.com/meltyshev/mikrio) for the
API, compatibility, platform limits, and backup and recovery.

Licensed under Apache-2.0. Native dependencies have separate bundled notices.
