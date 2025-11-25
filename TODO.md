- [x] get turso_core to run (with respect to the Rust bindings)
sqlite database locally with .db file

- [x] refactor io_uring with enum to minimize indirection but allow us to inject
our own completion variant without patching turso_core too much

- [ ] impl basic async logic with custom enums (tcp server)

- [ ] Generic IO for app
