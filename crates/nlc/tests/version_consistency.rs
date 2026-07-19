//! Guards against `Cargo.toml`'s workspace version and `CHANGELOG.md`'s
//! latest entry drifting apart — run by `cargo test --workspace` (and thus
//! CI) on every push, not just at release time.

const CHANGELOG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../CHANGELOG.md"));

#[test]
fn changelog_top_version_matches_cargo_version() {
    let header = CHANGELOG
        .lines()
        .find_map(|line| line.strip_prefix("## [")?.split(']').next())
        .expect("CHANGELOG.md has no \"## [x.y.z]\" version header");

    assert_eq!(
        header,
        env!("CARGO_PKG_VERSION"),
        "CHANGELOG.md's latest version ([{header}]) doesn't match Cargo.toml's workspace \
         version ({}) — bump whichever one is behind.",
        env!("CARGO_PKG_VERSION"),
    );
}
