- [x] get turso_core to run (with respect to the Rust bindings)
sqlite database locally with .db file

- [x] refactor io_uring with enum to minimize indirection but allow us to inject
our own completion variant without patching turso_core too much

- [x] impl basic async logic with custom enums (tcp server)

- [x] impl custom lifetime based waker (nonstd) and run queue (crossbeam)
 
- [x] impl send program

- [ ] impl basic tcp protocol for messaging (websocket with handshake?)
