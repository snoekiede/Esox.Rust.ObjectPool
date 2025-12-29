# Security Policy

## Supported Versions

We release patches for security vulnerabilities. The following versions are currently being supported with security updates:

| Version | Supported          |
| ------- | ------------------ |
| 4.0.x   | :white_check_mark: |
| < 4.0   | :x:                |

## Reporting a Vulnerability

We take the security of ObjectPool seriously. If you believe you have found a security vulnerability, please report it to us as described below.

### Please Do Not

- **Do not** open a public GitHub issue for security vulnerabilities
- **Do not** disclose the vulnerability publicly until it has been addressed

### Please Do

**Report security vulnerabilities via email to:** [info@esoxsolutions.nl](mailto:info@esoxsolutions.nl)

Include the following information in your report:

- **Type of vulnerability** (e.g., buffer overflow, SQL injection, cross-site scripting, etc.)
- **Full paths of source file(s)** related to the manifestation of the vulnerability
- **Location of the affected source code** (tag/branch/commit or direct URL)
- **Step-by-step instructions** to reproduce the issue
- **Proof-of-concept or exploit code** (if possible)
- **Impact of the vulnerability**, including how an attacker might exploit it

### What to Expect

After you submit a vulnerability report, you can expect:

1. **Acknowledgment**: We will acknowledge receipt of your vulnerability report within 48 hours.

2. **Assessment**: We will investigate and assess the vulnerability within 5 business days.

3. **Updates**: We will keep you informed about our progress toward a fix and full announcement.

4. **Fix and Disclosure**: 
   - We will work on a fix and release it as quickly as possible
   - Once a fix is available, we will coordinate disclosure timing with you
   - We aim to disclose within 90 days of the initial report

5. **Credit**: We will credit you in the security advisory (unless you prefer to remain anonymous).

## Security Update Process

When a security vulnerability is confirmed:

1. A fix will be developed and tested
2. A new version will be released with the fix
3. A security advisory will be published
4. The vulnerability details will be disclosed responsibly

## Security Best Practices for Users

To ensure the security of your applications using ObjectPool:

### Dependency Management

- **Keep dependencies up to date**: Regularly update to the latest version
  ```bash
  cargo update
  ```
- **Audit dependencies**: Use `cargo audit` to check for known vulnerabilities
  ```bash
  cargo install cargo-audit
  cargo audit
  ```

### Secure Configuration

- **Limit pool sizes**: Set appropriate `max_pool_size` to prevent resource exhaustion
- **Use timeouts**: Configure `get_timeout_ms` to prevent indefinite blocking
- **Validate objects**: Use validation functions to ensure object integrity
- **Monitor metrics**: Regularly check pool health and metrics for anomalies

### Thread Safety

- **Trust the type system**: ObjectPool enforces `Send + Sync` bounds at compile time
- **Avoid unsafe code**: Do not use `unsafe` blocks when interacting with pools
- **Test concurrency**: Thoroughly test multi-threaded usage patterns

### Error Handling

- **Handle errors gracefully**: Always check `Result` types and handle errors appropriately
- **Avoid panics**: Use `try_*` methods when you need non-panicking behavior
- **Log security events**: Log unusual pool behavior or errors for monitoring

### Production Deployment

- **Enable circuit breaker**: Use circuit breaker pattern to prevent cascading failures
  ```rust
  let config = PoolConfiguration::default()
      .with_circuit_breaker_threshold(5);
  ```
- **Set eviction policies**: Configure TTL to prevent stale objects
  ```rust
  let config = PoolConfiguration::default()
      .with_ttl(Duration::from_secs(300));
  ```
- **Monitor health**: Regularly check pool health status
  ```rust
  let health = pool.get_health_status();
  if health.utilization > 0.9 {
      // Alert or scale
  }
  ```

## Known Security Considerations

### Resource Exhaustion

- **Risk**: Unlimited object creation can exhaust system resources
- **Mitigation**: Always set `max_pool_size` and `max_active_objects` limits

### Denial of Service

- **Risk**: Callers waiting indefinitely for objects can lead to thread starvation
- **Mitigation**: Configure appropriate timeouts using `get_timeout_ms`

### Object State

- **Risk**: Reused objects may contain sensitive data from previous use
- **Mitigation**: Implement proper reset/cleanup in factory functions or use validation

### Memory Safety

- **Risk**: Rust's memory safety guarantees can be bypassed with `unsafe`
- **Mitigation**: This library avoids `unsafe` code; users should do the same

## Third-Party Dependencies

ObjectPool relies on several well-maintained dependencies:

- **tokio**: Industry-standard async runtime
- **crossbeam**: Well-audited concurrent data structures
- **dashmap**: Concurrent hash map with strong safety guarantees
- **parking_lot**: High-performance synchronization primitives

We regularly monitor these dependencies for security advisories and update as needed.

## Vulnerability Disclosure Policy

We follow a **coordinated disclosure** approach:

- We request that security researchers give us reasonable time to address vulnerabilities before public disclosure
- We will work with you to understand and address the issue
- We will publicly acknowledge your responsible disclosure (with your permission)
- We commit to transparency in our security advisories

## Security Testing

We encourage security testing of ObjectPool:

- **Fuzzing**: Feel free to fuzz the library and report any crashes or hangs
- **Concurrency testing**: Test for race conditions and deadlocks
- **Resource testing**: Test for memory leaks and resource exhaustion
- **Boundary testing**: Test edge cases and error conditions

Please report any findings via email to [info@esoxsolutions.nl](mailto:info@esoxsolutions.nl).

## Compliance

While ObjectPool is a general-purpose library, we recognize it may be used in regulated environments:

- **No warranty**: See LICENSE for details on warranty limitations
- **User responsibility**: Users are responsible for compliance with applicable regulations
- **Audit trails**: Consider implementing logging for compliance requirements

## Contact

For security-related questions or concerns:

- **Email**: [info@esoxsolutions.nl](mailto:info@esoxsolutions.nl)
- **Subject line**: Include "SECURITY" in the subject line

---

Thank you for helping keep ObjectPool and its users safe!
