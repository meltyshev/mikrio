# @mikrio/node

Embedded object storage for Node.js and TypeScript, with an optional
S3-compatible HTTP listener.

```typescript
import { Store } from "@mikrio/node";

const store = await Store.open({ directory: "./storage" });

try {
  await store.createBucket({ bucket: "photos" });
  await store.putObject({
    bucket: "photos",
    key: "hello.txt",
    body: Buffer.from("Hello, world!"),
  });
} finally {
  await store.close();
}
```

ESM and CommonJS share the same native implementation. TypeScript declarations
are included. Native packages are selected for the operating system and CPU;
there is no install script or compiler fallback. Keep the matching SQLite
companion with its native package.

See the [project documentation](https://github.com/meltyshev/mikrio) for the
API, compatibility, platform limits, and backup and recovery.

Licensed under Apache-2.0. Native dependencies have separate bundled notices.
