//! Structural checks on `README.md`, which is included as the crate's documentation.
//!
//! A fence that is not closed with ```` ``` ```` alone silently swallows the code blocks that follow:
//! they stop being compiled as doctests and render as prose on GitHub and docs.rs.

use std::collections::BTreeSet;

/// Fence languages the README is allowed to use.
const LANGUAGES: [&str; 5] = ["rust", "rust,ignore", "sh", "toml", "text"];

/// One fenced code block.
struct Block {
    /// Line the opening fence sits on.
    opens: usize,
    /// Line the closing fence sits on.
    closes: usize,
    /// Language written on the opening fence, including any `,ignore` marker.
    language: String,
}

/// Pairs the fences of `readme`, asserting that each one is well formed.
fn blocks(readme: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (index, line) in readme.lines().enumerate() {
        let number = index + 1;
        let Some(rest) = line.strip_prefix("```") else {
            continue;
        };
        match open.take() {
            None => {
                let language = rest.trim().to_owned();
                assert!(
                    LANGUAGES.contains(&language.as_str()),
                    "README line {number}: the fence language {language:?} must be one of {LANGUAGES:?}"
                );
                open = Some((number, language));
            }
            Some((opens, language)) => {
                assert!(
                    rest.trim().is_empty(),
                    "README line {number}: the fence that opened at line {opens} must close with ``` alone, found {rest:?}"
                );
                blocks.push(Block {
                    opens,
                    closes: number,
                    language,
                });
            }
        }
    }
    assert!(
        open.is_none(),
        "README has an unclosed code fence at line {:?}",
        open.map(|(opens, _)| opens)
    );
    blocks
}

#[test]
fn readme_code_fences_are_well_formed() {
    let readme = include_str!("../README.md");
    let lines = readme.lines().collect::<Vec<_>>();
    let blocks = blocks(readme);
    assert!(
        blocks.len() >= 8,
        "the README should carry its quickstart and reference snippets, saw {}",
        blocks.len()
    );

    for block in &blocks {
        // The rendered document needs a blank line after a block before the next paragraph.
        if let Some(next) = lines.get(block.closes) {
            assert!(
                next.trim().is_empty(),
                "README line {}: expected a blank line after the block that opens at line {}",
                block.closes + 1,
                block.opens
            );
        }
    }

    // Every language the README uses is allowed, and it keeps both compiled and skipped Rust snippets.
    let languages = blocks
        .iter()
        .map(|block| block.language.as_str())
        .collect::<BTreeSet<_>>();
    assert!(languages.contains("rust"), "{languages:?}");
    assert!(languages.contains("rust,ignore"), "{languages:?}");
}

#[test]
fn readme_blocks_do_not_absorb_their_neighbours() {
    // Every block's closing fence must sit before the next block opens, which is what broke when a
    // closing fence shared a line with prose: the following snippet stopped being compiled.
    let readme = include_str!("../README.md");
    let blocks = blocks(readme);
    for pair in blocks.windows(2) {
        assert!(
            pair[0].closes < pair[1].opens,
            "the block opening at line {} closes at line {}, past the next block at line {}",
            pair[0].opens,
            pair[0].closes,
            pair[1].opens
        );
    }
    let rust_blocks = blocks
        .iter()
        .filter(|block| block.language.starts_with("rust"))
        .count();
    assert!(
        rust_blocks >= 6,
        "expected the README's Rust snippets, saw {rust_blocks}"
    );
}
