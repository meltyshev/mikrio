# Security

Do not report vulnerabilities, credentials, signed URLs, or production store
contents in a public issue.

Use the repository's [private vulnerability
reporting][private-vulnerability-report] when enabled. If that option is
unavailable, open an issue requesting a private contact without describing the
vulnerability or including sensitive material.

Include the affected package and version, operating system, runtime, impact,
prerequisites, and a minimal reproduction. Arrange any sensitive evidence
transfer privately. Preserve a failed store before attempting recovery; normal
opening can run maintenance and is not a read-only inspection.

Security fixes are prioritized on the current release line on a best-effort
basis. No response time or support for older release lines is guaranteed.

Native directory access is administrative authority. Protect the store and its
backups with operating-system permissions. S3 policies do not isolate a process
that can read or modify those files. TLS is provided by a separately configured
reverse proxy; the embedded listener does not provide native TLS.

[private-vulnerability-report]:
  https://github.com/meltyshev/mikrio/security/advisories/new
