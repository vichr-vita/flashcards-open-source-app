# Local RSA compatibility patch

This directory contains the published `webauthn-rs-core` 0.5.5 crate from
[kanidm/webauthn-rs](https://github.com/kanidm/webauthn-rs). Its source revision is
`d2c10d53ca5ef033d37ee6462e936e9eb72ad98c`, recorded in `.cargo_vcs_info.json`.
The original crate archive has SHA-256
`296d2d501feb715d80b8e186fb88bab1073bca17f460303a1013d17b673bea6a`.
The upstream source and this modification use the Mozilla Public License 2.0.
The complete license is in `LICENSE.md`, copied from the same upstream revision.

The only source change is in `src/crypto.rs`. The COSE RS256 parser accepts a
256-, 384-, or 512-byte modulus instead of requiring exactly 256 bytes. The previous
SimpleWebAuthn runtime accepted 3072-bit and 4096-bit credentials, and the
library's existing RSA key type and OpenSSL verifier support these sizes.
Weak and other unsupported modulus sizes remain rejected. The existing
algorithm, exponent, key validation, registration, user
verification, origin, RP ID, backup flags, counter, and signature checks remain
in the maintained library. The app does not implement a signature verifier.

`RSA_MODULUS.patch` records the full source change. To reproduce it, download
`https://static.crates.io/crates/webauthn-rs-core/webauthn-rs-core-0.5.5.crate`,
verify the archive checksum above, extract it, and run this inside the extracted
crate directory:

```sh
patch -p1 < /path/to/vendor/webauthn-rs-core/RSA_MODULUS.patch
```

The Cargo manifest selects this directory through `[patch.crates-io]`. Other
files retain their published contents, apart from this note, the patch file,
and the upstream license. The auth HTTP integration imports old ECDSA and RSA
keys, enrolls new 3072-bit and 4096-bit RSA keys, verifies their real signatures,
and rejects unsupported algorithms and malformed keys. Run it through the
disposable stack check in `scripts/check.sh`.
