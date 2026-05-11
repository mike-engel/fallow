mod astro;
mod css;
mod graphql;
mod js_ts;
mod mdx;
mod regex_compile;
mod sfc;

use std::path::Path;

use fallow_types::discover::FileId;
use fallow_types::extract::ModuleInfo;

use crate::parse::parse_source_to_module;

/// Shared test helper: parse TypeScript source and return `ModuleInfo`.
pub fn parse_ts(source: &str) -> ModuleInfo {
    parse_source_to_module(FileId(0), Path::new("test.ts"), source, 0, false)
}

/// Shared test helper: parse TypeScript source with complexity metrics.
pub fn parse_ts_with_complexity(source: &str) -> ModuleInfo {
    parse_source_to_module(FileId(0), Path::new("test.ts"), source, 0, true)
}

/// Shared test helper: parse TSX source and return `ModuleInfo`.
pub fn parse_tsx(source: &str) -> ModuleInfo {
    parse_source_to_module(FileId(0), Path::new("test.tsx"), source, 0, false)
}

#[test]
fn parses_glimmer_typescript_as_typescript() {
    let info = parse_source_to_module(
        FileId(0),
        Path::new("component.gts"),
        "import type Service from './service';\nexport type ServiceRef = Service;\n",
        0,
        false,
    );

    assert_eq!(info.imports.len(), 1);
    assert_eq!(info.imports[0].source, "./service");
    assert!(info.imports[0].is_type_only);
    assert!(
        info.exports
            .iter()
            .any(|export| export.name.matches_str("ServiceRef"))
    );
}

#[test]
fn glimmer_template_only_pascal_tag_credits_import() {
    let info = parse_source_to_module(
        FileId(0),
        Path::new("app.gts"),
        "import HelloWorld from './hello-world';\nimport { greeting } from './lib';\n\
         <template><HelloWorld @msg={{greeting}} /></template>\n",
        0,
        false,
    );

    assert!(
        info.unused_import_bindings.is_empty(),
        "expected HelloWorld and greeting to be credited via the <template> block, \
         but unused_import_bindings = {:?}",
        info.unused_import_bindings,
    );
}

#[test]
fn glimmer_dotted_template_reference_emits_member_access() {
    let info = parse_source_to_module(
        FileId(0),
        Path::new("app.gts"),
        "import * as utils from './utils';\n<template>{{utils.formatDate value}}</template>\n",
        0,
        false,
    );

    assert!(info.unused_import_bindings.is_empty());
    assert!(
        info.member_accesses
            .iter()
            .any(|access| access.object == "utils" && access.member == "formatDate")
    );
}

#[test]
fn glimmer_import_used_only_inside_template_is_not_flagged() {
    // Regression for the original symptom: an import that is referenced ONLY
    // inside the template block was previously surfaced as `unused-import`
    // because the template body is blanked before the JS parse.
    let info = parse_source_to_module(
        FileId(0),
        Path::new("counter.gts"),
        "import { capitalize } from './helpers';\n\
         <template>{{capitalize name}}</template>\n",
        0,
        false,
    );

    assert!(info.unused_import_bindings.is_empty());
}

// ── negative cases: confirm the scanner does NOT over-credit ───────────
//
// The trio above only proves the credit path works. These tests fail the
// suite if the scanner regresses into crediting bindings it shouldn't (or
// if `info.unused_import_bindings` ever gets stubbed back to an empty set
// by mistake): each one declares an import that is genuinely unreachable
// and asserts it surfaces in `unused_import_bindings`.

fn assert_unused(info: &ModuleInfo, expected: &[&str]) {
    let mut actual: Vec<&str> = info
        .unused_import_bindings
        .iter()
        .map(String::as_str)
        .collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(
        actual, expected,
        "unused_import_bindings did not match expected set"
    );
}

#[test]
fn glimmer_import_referenced_nowhere_is_flagged_unused() {
    let info = parse_source_to_module(
        FileId(0),
        Path::new("app.gts"),
        "import { unused } from './lib';\n\
         <template>hello world</template>\n",
        0,
        false,
    );
    assert_unused(&info, &["unused"]);
}

#[test]
fn glimmer_import_referenced_only_via_this_dot_in_template_is_flagged() {
    // `this.greeting` reads a class property — even when `greeting` happens
    // to also be an imported binding name, the template scanner must not
    // credit the import.
    let info = parse_source_to_module(
        FileId(0),
        Path::new("app.gts"),
        "import { greeting } from './lib';\n\
         <template>{{this.greeting}}</template>\n",
        0,
        false,
    );
    assert_unused(&info, &["greeting"]);
}

#[test]
fn glimmer_import_referenced_only_via_arg_in_template_is_flagged() {
    // `@name` is a template argument, not a module-scope binding. An import
    // named `name` is genuinely unused here.
    let info = parse_source_to_module(
        FileId(0),
        Path::new("app.gts"),
        "import { name } from './lib';\n\
         <template>{{@name}}</template>\n",
        0,
        false,
    );
    assert_unused(&info, &["name"]);
}

#[test]
fn glimmer_import_shadowing_builtin_helper_is_flagged() {
    // `each` is a Glimmer built-in helper keyword; the scanner must NEVER
    // resolve it to an import binding, even if the user did `import { each }`.
    // (Built-ins like `if` / `let` are reserved words and can't be import
    // identifiers, so `each` is the realistic regression to lock in.)
    let info = parse_source_to_module(
        FileId(0),
        Path::new("app.gts"),
        "import { each } from './lib';\n\
         <template>{{#each items as |x|}}{{x}}{{/each}}</template>\n",
        0,
        false,
    );
    assert_unused(&info, &["each"]);
}

#[test]
fn glimmer_import_shadowed_by_block_param_is_flagged() {
    // `as |item|` introduces a template-scope local. References to `item`
    // inside the block resolve to the local, NOT to the same-named import,
    // so the import must surface as unused.
    let info = parse_source_to_module(
        FileId(0),
        Path::new("app.gts"),
        "import { item } from './lib';\n\
         <template>{{#each items as |item|}}{{item}}{{/each}}</template>\n",
        0,
        false,
    );
    assert!(
        info.unused_import_bindings.iter().any(|b| b == "item"),
        "`item` should be flagged unused (shadowed by block param), \
         unused_import_bindings = {:?}",
        info.unused_import_bindings,
    );
}

#[test]
fn glimmer_mix_of_used_and_unused_imports_flags_only_the_unused() {
    let info = parse_source_to_module(
        FileId(0),
        Path::new("app.gts"),
        "import HelloWorld from './hello-world';\n\
         import { greeting } from './lib';\n\
         import { stale } from './lib';\n\
         <template><HelloWorld @msg={{greeting}} /></template>\n",
        0,
        false,
    );
    assert_unused(&info, &["stale"]);
}

#[test]
fn glimmer_strict_mode_helper_imports_from_ember_helper_are_credited() {
    // Strict-mode `.gts` requires `hash`, `array`, `concat`, `fn`, `mut`,
    // `get` to be imported from `@ember/helper`; using them in `<template>`
    // must keep the import credited. Regression for the case where these
    // names were misclassified as ambient built-ins and never credited.
    let info = parse_source_to_module(
        FileId(0),
        Path::new("form.gts"),
        "import { hash, array, concat, fn, get } from '@ember/helper';\n\
         <template>\n  \
           {{#let (hash a=(array 1 2) label=(concat \"x\" \"y\")) as |opts|}}\n    \
             <button {{on \"click\" (fn this.save opts)}}>{{get opts \"label\"}}</button>\n  \
           {{/let}}\n\
         </template>\n",
        0,
        false,
    );
    assert!(
        info.unused_import_bindings.is_empty(),
        "expected hash/array/concat/fn/get to be credited via the template, \
         unused_import_bindings = {:?}",
        info.unused_import_bindings,
    );
}

#[test]
fn glimmer_template_this_dot_member_emits_member_access() {
    // Real-world case (Customer.io `tests/utils/visual-workflow-builder/
    // components.gts`): a class with arrow-function fields whose ONLY
    // call-site is `<Child @prop={{this.field}}>` in the surrounding
    // `<template>` block. Without member-access emission for `this.field`,
    // unused-class-members flags those fields as unused even though the
    // template wires them into a child component.
    let info = parse_source_to_module(
        FileId(0),
        Path::new("test-vwb.gts"),
        "import Component from '@glimmer/component';\n\
         export class TestVWB extends Component {\n  \
           handleSelectedAction = (x) => { void x; };\n  \
           changeTab = (t) => { void t; };\n  \
           <template>\n    \
             <Child @onSelectedAction={{this.handleSelectedAction}} \
                    @changeTab={{this.changeTab}} />\n  \
           </template>\n\
         }\n",
        0,
        false,
    );
    let access_keys: Vec<(&str, &str)> = info
        .member_accesses
        .iter()
        .map(|a| (a.object.as_str(), a.member.as_str()))
        .collect();
    assert!(
        access_keys.contains(&("this", "handleSelectedAction")),
        "expected this.handleSelectedAction member-access from <template>; \
         got {access_keys:?}"
    );
    assert!(
        access_keys.contains(&("this", "changeTab")),
        "expected this.changeTab member-access from <template>; got {access_keys:?}"
    );
}

#[test]
fn glimmer_template_this_dot_member_records_access_with_zero_imports() {
    // Edge case the previous post-construction `apply_glimmer_template_usage`
    // got wrong: a `.gts` file with NO module-scope imports but a
    // `{{this.foo}}` template reference must still record the member access
    // so unused-class-members tracking sees template-only `this.*` uses.
    // The Angular-shaped fold-into-extractor path runs the scan even when
    // imports are empty (only the binding-credit branch is a no-op).
    let info = parse_source_to_module(
        FileId(0),
        Path::new("no-imports.gts"),
        "export class Widget {\n  \
           handleClick = () => {};\n  \
           <template>\n    \
             <button {{on \"click\" this.handleClick}}>x</button>\n  \
           </template>\n\
         }\n",
        0,
        false,
    );
    let access_keys: Vec<(&str, &str)> = info
        .member_accesses
        .iter()
        .map(|a| (a.object.as_str(), a.member.as_str()))
        .collect();
    assert!(
        access_keys.contains(&("this", "handleClick")),
        "this.handleClick must still be recorded as a member access when \
         the file has zero module-scope imports; got {access_keys:?}",
    );
}

#[test]
fn glimmer_file_without_template_still_flags_unused_imports() {
    // Sanity: the scanner only ever ADDS credit; on a `.gts` file with no
    // `<template>` block at all, an unused import must still surface.
    let info = parse_source_to_module(
        FileId(0),
        Path::new("plain.gts"),
        "import { unused } from './lib';\nexport const x = 1;\n",
        0,
        false,
    );
    assert_unused(&info, &["unused"]);
}
