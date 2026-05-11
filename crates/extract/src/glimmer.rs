//! Helpers for Glimmer component files (`.gts` / `.gjs`).

use std::ops::Range;
use std::path::Path;

/// Return `true` for Glimmer source files.
#[must_use]
pub fn is_glimmer_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "gts" || ext == "gjs")
}

/// Locate `<template>...</template>` block byte ranges. The returned ranges
/// span from the opening `<` of `<template` through the closing `>` of
/// `</template>`. An unclosed `<template` consumes from its start to the end
/// of the source. The byte offsets are stable and the same offsets used by
/// [`strip_glimmer_templates`] so callers can correlate the two passes.
#[must_use]
pub fn find_template_ranges(source: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 0;

    while let Some(relative_start) = source[cursor..].find("<template") {
        let start = cursor + relative_start;
        let end = source[start..]
            .find("</template>")
            .map_or(source.len(), |relative_end| {
                start + relative_end + "</template>".len()
            });
        ranges.push(start..end);
        if end >= source.len() {
            break;
        }
        cursor = end;
    }

    ranges
}

/// Blank Glimmer `<template>` blocks while preserving byte offsets and line
/// numbers for the JavaScript/TypeScript parser.
#[must_use]
pub fn strip_glimmer_templates(source: &str) -> Option<String> {
    let ranges = find_template_ranges(source);
    if ranges.is_empty() {
        return None;
    }

    let mut bytes = source.as_bytes().to_vec();
    for range in ranges {
        for byte in &mut bytes[range] {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }

    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_template_ranges_captures_all_blocks() {
        let source = "a<template>b\nc</template>d<template>e</template>f";
        let ranges = find_template_ranges(source);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&source[ranges[0].clone()], "<template>b\nc</template>");
        assert_eq!(&source[ranges[1].clone()], "<template>e</template>");
    }

    #[test]
    fn find_template_ranges_handles_unclosed_block() {
        let source = "<template>nope";
        let ranges = find_template_ranges(source);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], 0..source.len());
    }

    #[test]
    fn find_template_ranges_returns_empty_when_absent() {
        assert!(find_template_ranges("export const x = 1;").is_empty());
    }

    #[test]
    fn strips_template_blocks_and_preserves_newlines() {
        let source =
            "import x from './x';\n<template>\n  <x />\n</template>\nexport const y = x;\n";
        let stripped = strip_glimmer_templates(source).expect("template should be stripped");

        assert!(stripped.contains("import x from './x';"));
        assert!(stripped.contains("export const y = x;"));
        assert!(!stripped.contains("<template>"));
        assert_eq!(stripped.len(), source.len());
        assert_eq!(stripped.lines().count(), source.lines().count());
    }
}
