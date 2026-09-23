# Skadi

A unified, Rust-native replacement for the *arr stack

## Development

This project uses [angreal](https://github.com/angreal/angreal) for task automation.

### Prerequisites

- Rust 1.93+
- [angreal](https://github.com/angreal/angreal) (`pip install angreal`)
- [pre-commit](https://pre-commit.com/) (`pip install pre-commit`)

### Common Commands

```bash
# Run checks
angreal check all

# Run tests
angreal test unit
angreal test integration
angreal test coverage

# Build
angreal build
angreal build --release

# Version management
angreal version show
angreal version bump patch
```

## Project Structure

```
crates/
  skadi-core/    # Core library
  skadi-cli/     # CLI binary
```

## License

MIT OR Apache-2.0
