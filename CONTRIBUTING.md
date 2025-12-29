# Contributing to ObjectPool

Thank you for your interest in contributing to ObjectPool! This document provides guidelines and instructions for contributing to the project.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Making Changes](#making-changes)
- [Testing](#testing)
- [Documentation](#documentation)
- [Submitting Changes](#submitting-changes)
- [Code Style](#code-style)
- [Reporting Issues](#reporting-issues)

## Code of Conduct

This project adheres to the Contributor Covenant [Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code. Please report unacceptable behavior to [info@esoxsolutions.nl](mailto:info@esoxsolutions.nl).

## Getting Started

1. **Fork the repository** on GitHub
2. **Clone your fork** locally:
   ```bash
   git clone https://github.com/YOUR-USERNAME/objectpool.git
   cd objectpool
   ```
3. **Add the upstream repository**:
   ```bash
   git remote add upstream https://github.com/ORIGINAL-OWNER/objectpool.git
   ```

## Development Setup

### Prerequisites

- Rust 1.70 or later (install via [rustup](https://rustup.rs/))
- Cargo (comes with Rust)
- Git

### Building the Project

```bash
# Build the project
cargo build

# Build with optimizations
cargo build --release

# Run the example
cargo run --example basic
```

### Running Tests

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run only unit tests
cargo test --lib

# Run only doc tests
cargo test --doc

# Run specific test
cargo test test_name
```

## Making Changes

1. **Create a new branch** for your changes:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes** following the [code style guidelines](#code-style)

3. **Write or update tests** for your changes

4. **Update documentation** as needed

5. **Commit your changes** with clear, descriptive commit messages:
   ```bash
   git commit -m "Add feature: description of your changes"
   ```

### Commit Message Guidelines

- Use the present tense ("Add feature" not "Added feature")
- Use the imperative mood ("Move cursor to..." not "Moves cursor to...")
- Limit the first line to 72 characters or less
- Reference issues and pull requests liberally after the first line

Examples:
- `Fix: resolve deadlock in DynamicObjectPool`
- `Add: support for custom eviction policies`
- `Docs: update README with async examples`
- `Test: add coverage for circuit breaker transitions`

## Testing

All contributions must include appropriate tests:

### Unit Tests

- Add unit tests in the same file as the code using `#[cfg(test)]` modules
- Test both success and failure cases
- Test edge cases and boundary conditions
- Aim for high code coverage

Example:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_get_and_return() {
        // Test implementation
    }
}
```

### Doc Tests

- Add doc tests to demonstrate usage of public APIs
- Doc tests serve as both documentation and tests
- Include realistic examples that users can copy

Example:
```rust
/// Creates a new object pool.
///
/// # Examples
///
/// ```
/// use objectpool::ObjectPool;
/// let pool = ObjectPool::new(|| 42, 10).unwrap();
/// ```
pub fn new() -> Self {
    // Implementation
}
```

### Integration Tests

- Add integration tests in the `examples/` directory
- Demonstrate real-world usage scenarios
- Test interactions between multiple components

## Documentation

### Code Documentation

- Document all public APIs with doc comments (`///`)
- Include examples in doc comments when helpful
- Explain complex algorithms or non-obvious behavior
- Keep documentation up-to-date with code changes

### README and Guides

- Update README.md if adding new features
- Add examples to demonstrate new functionality
- Update IMPLEMENTATION.md for architectural changes
- Keep documentation clear and concise

## Submitting Changes

### Pull Request Process

1. **Update your branch** with the latest upstream changes:
   ```bash
   git fetch upstream
   git rebase upstream/main
   ```

2. **Push your changes** to your fork:
   ```bash
   git push origin feature/your-feature-name
   ```

3. **Create a pull request** on GitHub:
   - Provide a clear title and description
   - Reference any related issues
   - Describe what changed and why
   - Include screenshots or examples if applicable

4. **Address review feedback**:
   - Respond to comments promptly
   - Make requested changes
   - Push updates to the same branch

5. **Wait for approval**:
   - At least one maintainer must approve
   - All CI checks must pass
   - No merge conflicts

### Pull Request Checklist

Before submitting your PR, ensure:

- [ ] Code follows the project's style guidelines
- [ ] All tests pass (`cargo test`)
- [ ] New code has adequate test coverage
- [ ] Documentation is updated
- [ ] Commit messages are clear and descriptive
- [ ] No compiler warnings (`cargo clippy`)
- [ ] Code is formatted (`cargo fmt`)
- [ ] CHANGELOG.md is updated (if applicable)

## Code Style

### Rust Style Guidelines

This project follows standard Rust conventions:

1. **Formatting**: Use `rustfmt` for consistent formatting
   ```bash
   cargo fmt
   ```

2. **Linting**: Use `clippy` to catch common mistakes
   ```bash
   cargo clippy -- -D warnings
   ```

3. **Naming Conventions**:
   - Use `snake_case` for functions, variables, and modules
   - Use `CamelCase` for types and traits
   - Use `SCREAMING_SNAKE_CASE` for constants
   - Use descriptive names that convey purpose

4. **Error Handling**:
   - Use `Result<T, E>` for operations that can fail
   - Use `Option<T>` for values that may be absent
   - Prefer `?` operator for error propagation
   - Provide meaningful error messages

5. **Thread Safety**:
   - Mark types as `Send` and `Sync` when appropriate
   - Document thread safety guarantees
   - Use appropriate synchronization primitives

6. **Performance**:
   - Avoid unnecessary allocations
   - Use appropriate data structures
   - Document performance characteristics
   - Add benchmarks for performance-critical code

### Code Organization

- Keep modules focused and cohesive
- Limit file size to ~500 lines when practical
- Group related functionality together
- Use clear module hierarchies

## Reporting Issues

### Bug Reports

When reporting bugs, please include:

- **Description**: Clear description of the issue
- **Steps to reproduce**: Minimal code example
- **Expected behavior**: What you expected to happen
- **Actual behavior**: What actually happened
- **Environment**: Rust version, OS, relevant dependencies
- **Stack trace**: If applicable

### Feature Requests

When requesting features, please include:

- **Use case**: Why this feature is needed
- **Proposed solution**: How you envision it working
- **Alternatives**: Other approaches you've considered
- **Examples**: Code examples if helpful

### Questions

For questions about usage:

- Check existing documentation and examples
- Search existing issues
- Create a new issue with the "question" label

## Development Tips

### Useful Commands

```bash
# Check for errors without building
cargo check

# Run clippy for linting
cargo clippy

# Format code
cargo fmt

# Build documentation
cargo doc --open

# Run benchmarks (if available)
cargo bench

# Check for outdated dependencies
cargo outdated
```

### Debugging

- Use `dbg!()` macro for quick debugging
- Use `println!()` for simple output
- Use proper logging with the `log` crate for production code
- Run tests with `--nocapture` to see output

### Performance Testing

- Write benchmarks for performance-critical code
- Profile with `cargo flamegraph` or `perf`
- Test under realistic load conditions

## Questions?

If you have questions about contributing, feel free to:

- Open an issue with the "question" label
- Contact the maintainers at [info@esoxsolutions.nl](mailto:info@esoxsolutions.nl)

Thank you for contributing to ObjectPool! 🎉
