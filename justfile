lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

fmt:
    cargo fmt

# The guide needs mdBook: `cargo install mdbook --version 0.5.4 --locked`, the version CI uses.
book:
    mdbook build book

book-serve:
    mdbook serve book --open
