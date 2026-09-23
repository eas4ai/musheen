# OpenDAL SFTP Credential Blocker

OpenDAL 0.59.3's SFTP builder accepts an OpenSSH private-key **file path** and
the OpenSSH `known_hosts` policies, but it cannot accept password or private-key
bytes obtained from Secret Service. It also cannot enforce Musheen's configured
SHA-256 host-key pin. Writing Secret Service material to a temporary file would
violate SYS-024 and silently weakening host verification would violate SYS-023.

DEP-012 therefore permits the SFTP adapter to use `russh`/`russh-sftp` for the
credentialed and pinned-host-key paths. OpenDAL remains the implementation for
FTP, FTPS, WebDAV, HTTP, and SFTP profiles whose security configuration can be
represented faithfully. The fallback must preserve the same bounded paging,
pooling, cancellation, and error-category contracts.

## FTPS certificate pins

OpenDAL 0.59.3 also hard-wires the platform root store in its FTPS connection
manager. The builder exposes no TLS connector or certificate-verifier hook, so
it cannot enforce an exact leaf-certificate pin. The adapter rejects pinned
FTPS profiles with `Unsupported` instead of falling back to system roots. A
future implementation must add an upstream connector hook or a complete custom
FTPS service that pins both the control and passive data connections. Pinning
only the control connection would leave file contents on an unverified channel.
