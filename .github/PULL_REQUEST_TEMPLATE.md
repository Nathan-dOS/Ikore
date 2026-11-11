## What changed and why

<!-- Describe the change and the problem it solves. Reference the issue, for example: Closes #12 -->

## How to test

<!-- Commands and steps a reviewer can follow locally. -->

## Checklist

- [ ] `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings` pass.
- [ ] The tests for the changed area pass locally (`cargo test-core`, `cargo test-ui`, ...). Cross-cutting changes also ran `cargo test-all`.
- [ ] New migrations, contracts or dependencies are justified and covered by tests.
- [ ] If `Dockerfile`, `docker-compose.yml`, `.env.example` or the workflows changed, the Docker build still works and `README.md` is updated.
- [ ] If `third_party/gpui` changed, there is a new entry in `third_party/gpui/IKORE-PATCHES.md`.
- [ ] No secret, personal path or private data is included.
- [ ] If an AI tool wrote a relevant part of this change, it is mentioned above.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the full guidelines.
