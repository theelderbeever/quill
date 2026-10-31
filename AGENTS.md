## Code principles

- Favor low overhead on streaming paths. Prefer borrows to ownership and avoid cloning
  or allocating on hot paths unless measurements justify it.
- Write straightforward, idiomatic Rust. Use `?` for propagation; use an explicit
  `match` on `Result` only when the error is actually handled.
- Put operations on one type in that type's `impl`; reserve free functions for
  genuinely cross-type helpers.
- Parse, don't validate. An invariant the code relies on — sortedness, a checked range,
  which of two lookalike byte strings this is — belongs in a newtype whose constructor is
  the only way to establish it, not in a comment re-checked at each use. Consider a `String`
  which cannot be empty, construct `NonEmptyString`. A newtype that removes no invariant is ceremony.
- Do not derive `Default` for a type whose empty or zero value carries a domain meaning.
- Do not derive both `Serialize/Deserialize` for a type which is not reversible.
  Derive only the intended variant.
- Prefer generic library seams over application-specific duplication, but do not add
  clever abstractions without a demonstrated need.
- Avoid magic values. Ask whether operational behavior belongs in
  `quill/src/lib.rs` and the generated configuration schema.
- Public interfaces should include useful doctests. Keep comments terse and focused on
  constraints or reasoning the code cannot express.
- Keep secrets secret even in memory. A database password should never be exposed in logs or elsewhere.
  Carry `SecretString` around until the exact location it needs used.
- Do not use `anyhow::Result` anywhere except `quill` or `quill-tui`. Library crates always return actionable error types.

## Crates

Crate boundaries should be based on domain as well as compilation boundaries.

- `quill` is the binary crate where the cli, figment configuration, and application wiring live. Top of the tree.
- `quill-core` defines the core functionality that is expected to be used throughout other crates. Bottom of the tree.

## Development Scripts

- Shell scripts for development work should be simple and direct. They do not need to handle every edgecase under the sun.
