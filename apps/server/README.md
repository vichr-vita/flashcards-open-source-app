# Private browser server

`lingvichr` runs the private browser installation with Axum, Tokio, and SQLx.
It retains the installed HTTP contracts and PostgreSQL schema. AWS, native,
and admin services remain in their existing source directories.

Read [the private stack guide](../../docs/private-rust-stack.md) for builds,
runtime configuration, passkey administration, and database preservation.
Read [local checks](../../docs/local-checks.md) to run the PostgreSQL and browser
fixtures. The Astro documentation is in `apps/docs`.

```sh
cargo build --locked
target/debug/lingvichr --help
pnpm generate:types
bash scripts/check.sh
```

Use restricted backend and auth roles for serving requests. The owner role is
only for explicit migration and account commands. Startup does not migrate the
database. Existing SQL migrations and their full-filename ledger remain intact.
